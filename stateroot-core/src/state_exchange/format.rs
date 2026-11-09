use super::ExchangeError;
use git2::{ObjectType, Odb, Oid, Repository};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::Path;

pub const BUNDLE_MAGIC: &[u8; 8] = b"SRFSBND2";
pub const BUNDLE_SCHEMA: &str = "stateroot.srfs.bundle.v2";
pub const MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
pub const ARTIFACT_BYTES: u64 = 1024 * 1024;
pub const OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
pub const BUNDLE_BYTES: u64 = 10_u64 * 1024 * 1024 * 1024;
pub const FRAME_NAME_BYTES: u64 = 128;
pub const OBJECT_COUNT: usize = 250_000;
pub const ROOT_COUNT: usize = 250_000;
pub const ARTIFACT_COUNT: usize = 500_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BundleMode {
    Download,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleLimits {
    pub manifest_bytes: u64,
    pub artifact_bytes: u64,
    pub object_bytes: u64,
    pub bundle_bytes: u64,
    pub frame_name_bytes: u64,
    pub object_count: usize,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            manifest_bytes: MANIFEST_BYTES,
            artifact_bytes: ARTIFACT_BYTES,
            object_bytes: OBJECT_BYTES,
            bundle_bytes: BUNDLE_BYTES,
            frame_name_bytes: FRAME_NAME_BYTES,
            object_count: OBJECT_COUNT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootMetadata {
    pub oid: String,
    pub manifest: Option<String>,
    pub transitions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuxiliaryRef {
    #[serde(rename = "ref")]
    pub reference: String,
    pub oid: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmittedRef {
    #[serde(rename = "ref")]
    pub reference: String,
    pub oid: Option<String>,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDescriptor {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub schema: String,
    pub mode: BundleMode,
    pub project_id: String,
    pub root_oid: String,
    pub roots: Vec<RootMetadata>,
    pub auxiliary_refs: Vec<AuxiliaryRef>,
    pub omissions: Vec<OmittedRef>,
    pub artifacts: Vec<ArtifactDescriptor>,
    pub object_count: usize,
    pub limits: BundleLimits,
}

pub fn invalid(message: impl Into<String>) -> ExchangeError {
    ExchangeError::Invalid(message.into())
}

pub fn parse_oid(text: &str) -> Result<Oid, ExchangeError> {
    if text.len() != 40
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(format!(
            "OID is not 40 lowercase hex characters: {text}"
        )));
    }
    Oid::from_str(text).map_err(ExchangeError::from)
}

pub fn frame_type(kind: ObjectType) -> Result<u8, ExchangeError> {
    match kind {
        ObjectType::Blob => Ok(1),
        ObjectType::Tree => Ok(2),
        ObjectType::Commit => Ok(3),
        other => Err(invalid(format!("unsupported Git frame kind {other}"))),
    }
}

pub fn object_kind(frame: u8) -> Result<ObjectType, ExchangeError> {
    match frame {
        1 => Ok(ObjectType::Blob),
        2 => Ok(ObjectType::Tree),
        3 => Ok(ObjectType::Commit),
        _ => Err(invalid("unknown Git frame type")),
    }
}

pub fn valid_artifact_name(name: &str) -> bool {
    if let Some(oid) = name
        .strip_prefix("root/")
        .and_then(|n| n.strip_suffix(".json"))
    {
        return parse_oid(oid).is_ok();
    }
    if let Some(id) = name
        .strip_prefix("transition/")
        .and_then(|n| n.strip_suffix(".json"))
    {
        return uuid::Uuid::parse_str(id).is_ok();
    }
    false
}

pub fn valid_auxiliary(reference: &str, kind: &str) -> bool {
    if let Some(id) = reference.strip_prefix("refs/stateroot/verification/") {
        return kind == "tree"
            && id
                .strip_prefix("v_")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    }
    if let Some(oid) = reference.strip_prefix("refs/stateroot/roots/") {
        return kind == "commit" && parse_oid(oid).is_ok();
    }
    false
}

pub fn validate_manifest(manifest: &BundleManifest) -> Result<(), ExchangeError> {
    if manifest.schema != BUNDLE_SCHEMA {
        return Err(invalid(format!(
            "unsupported exchange schema {}",
            manifest.schema
        )));
    }
    if manifest.mode != BundleMode::Download {
        return Err(invalid("portable exchange requires download mode"));
    }
    let identity = manifest.project_id.as_str();
    if identity.is_empty()
        || identity.len() > 128
        || !identity
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        || !identity.as_bytes()[0].is_ascii_alphanumeric()
    {
        return Err(invalid("invalid project identity"));
    }
    parse_oid(&manifest.root_oid)?;
    if manifest.roots.len() > ROOT_COUNT
        || manifest.auxiliary_refs.len() > ROOT_COUNT
        || manifest.omissions.len() > ROOT_COUNT
        || manifest.artifacts.len() > ARTIFACT_COUNT
        || manifest.object_count == 0
        || manifest.object_count > OBJECT_COUNT
    {
        return Err(invalid("manifest count bound exceeded"));
    }
    let defaults = BundleLimits::default();
    if manifest.limits.manifest_bytes != defaults.manifest_bytes
        || manifest.limits.artifact_bytes != defaults.artifact_bytes
        || manifest.limits.object_bytes != defaults.object_bytes
        || manifest.limits.bundle_bytes != defaults.bundle_bytes
        || manifest.limits.frame_name_bytes != defaults.frame_name_bytes
        || manifest.limits.object_count != defaults.object_count
    {
        return Err(invalid("unsupported exchange limits"));
    }
    let mut roots = BTreeSet::new();
    for root in &manifest.roots {
        if !roots.insert(parse_oid(&root.oid)?) {
            return Err(invalid("duplicate root metadata"));
        }
        if root
            .manifest
            .as_deref()
            .is_some_and(|name| !valid_artifact_name(name))
            || root
                .transitions
                .iter()
                .any(|name| !valid_artifact_name(name))
            || root.transitions.len() > 16
        {
            return Err(invalid("invalid root artifact reference"));
        }
    }
    let mut refs = BTreeSet::new();
    for auxiliary in &manifest.auxiliary_refs {
        parse_oid(&auxiliary.oid)?;
        if !valid_auxiliary(&auxiliary.reference, &auxiliary.kind)
            || auxiliary
                .reference
                .strip_prefix("refs/stateroot/roots/")
                .is_some_and(|oid| oid != auxiliary.oid)
            || !refs.insert(auxiliary.reference.clone())
        {
            return Err(invalid("invalid or duplicate auxiliary reference"));
        }
    }
    for omission in &manifest.omissions {
        if omission.status.trim().is_empty()
            || !valid_auxiliary(&omission.reference, &omission.kind)
            || omission
                .reference
                .strip_prefix("refs/stateroot/roots/")
                .is_some_and(|oid| omission.oid.as_deref() != Some(oid))
            || !refs.insert(omission.reference.clone())
            || omission
                .oid
                .as_deref()
                .is_some_and(|oid| parse_oid(oid).is_err())
        {
            return Err(invalid("invalid or duplicate omitted reference"));
        }
    }
    let mut artifacts = BTreeSet::new();
    for artifact in &manifest.artifacts {
        if !valid_artifact_name(&artifact.name)
            || artifact.size > ARTIFACT_BYTES
            || artifact.sha256.len() != 64
            || !artifact
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !artifacts.insert(artifact.name.clone())
        {
            return Err(invalid("invalid or duplicate artifact descriptor"));
        }
    }
    Ok(())
}

pub fn write_manifest<W: Write>(
    writer: &mut W,
    manifest: &BundleManifest,
) -> Result<(), ExchangeError> {
    validate_manifest(manifest)?;
    let bytes = serde_json::to_vec(manifest)?;
    if bytes.len() as u64 > MANIFEST_BYTES {
        return Err(invalid("manifest exceeds v2 byte bound"));
    }
    writer.write_all(BUNDLE_MAGIC)?;
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    Ok(())
}

pub fn read_manifest<R: Read>(reader: &mut R) -> Result<BundleManifest, ExchangeError> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != BUNDLE_MAGIC {
        return Err(invalid("unsupported exchange magic"));
    }
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as u64;
    if length > MANIFEST_BYTES {
        return Err(invalid("manifest exceeds v2 byte bound"));
    }
    let mut bytes = vec![0; length as usize];
    reader.read_exact(&mut bytes)?;
    let manifest: BundleManifest = serde_json::from_slice(&bytes)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn write_frame_header<W: Write>(
    writer: &mut W,
    frame: u8,
    name: &str,
    payload: u64,
) -> Result<(), ExchangeError> {
    if name.len() as u64 > FRAME_NAME_BYTES || payload > OBJECT_BYTES {
        return Err(invalid("frame exceeds v2 bounds"));
    }
    writer.write_all(&[frame, 0, 0, 0])?;
    writer.write_all(&(name.len() as u32).to_be_bytes())?;
    writer.write_all(&payload.to_be_bytes())?;
    writer.write_all(name.as_bytes())?;
    Ok(())
}

pub fn read_frame_header<R: Read>(reader: &mut R) -> Result<(u8, String, u64), ExchangeError> {
    let mut header = [0u8; 16];
    reader.read_exact(&mut header)?;
    if header[1..4] != [0, 0, 0] {
        return Err(invalid("nonzero reserved frame bytes"));
    }
    let frame = header[0];
    let name_length = u32::from_be_bytes(header[4..8].try_into().unwrap()) as u64;
    let payload = u64::from_be_bytes(header[8..16].try_into().unwrap());
    if name_length > FRAME_NAME_BYTES || payload > OBJECT_BYTES {
        return Err(invalid("frame exceeds v2 bounds"));
    }
    let mut name = vec![0; name_length as usize];
    reader.read_exact(&mut name)?;
    let name = String::from_utf8(name).map_err(|_| invalid("frame name is not UTF-8"))?;
    Ok((frame, name, payload))
}

pub fn copy_exact<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    length: u64,
) -> Result<u64, ExchangeError> {
    let copied = std::io::copy(&mut reader.take(length), writer)?;
    if copied != length {
        return Err(invalid("truncated frame payload"));
    }
    Ok(copied)
}

pub fn write_object<W: Write>(
    writer: &mut W,
    repo: &Repository,
    oid: Oid,
    expected: ObjectType,
) -> Result<(), ExchangeError> {
    let odb = repo.odb()?;
    let (mut reader, size, kind) = odb.reader(oid)?;
    if kind != expected {
        return Err(invalid(format!("Git object kind mismatch for {oid}")));
    }
    if size as u64 > OBJECT_BYTES {
        return Err(invalid("Git object exceeds v2 bound"));
    }
    write_frame_header(writer, frame_type(kind)?, &oid.to_string(), size as u64)?;
    copy_exact(&mut reader, writer, size as u64)?;
    Ok(())
}

pub fn read_object<R: Read>(
    reader: &mut R,
    odb: &Odb,
    kind: ObjectType,
    name: &str,
    length: u64,
) -> Result<Oid, ExchangeError> {
    let expected = parse_oid(name)?;
    let mut writer = odb.writer(length as usize, kind)?;
    copy_exact(reader, &mut writer, length)?;
    let actual = writer.finalize()?;
    if actual != expected {
        return Err(invalid(format!("Git OID mismatch for {expected}")));
    }
    Ok(actual)
}

pub fn write_artifact<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    length: u64,
) -> Result<String, ExchangeError> {
    if length > ARTIFACT_BYTES {
        return Err(invalid("artifact exceeds v2 bound"));
    }
    let mut limited = reader.take(length);
    let mut hasher = Sha256::new();
    let copied = std::io::copy(
        &mut limited,
        &mut HashWriter {
            writer,
            hasher: &mut hasher,
        },
    )?;
    if copied != length {
        return Err(invalid("truncated artifact payload"));
    }
    Ok(format!("{:x}", hasher.finalize()))
}

struct HashWriter<'a, W: Write> {
    writer: &'a mut W,
    hasher: &'a mut Sha256,
}

impl<W: Write> Write for HashWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

pub fn read_artifact(path: &Path) -> Result<Vec<u8>, ExchangeError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > ARTIFACT_BYTES {
        return Err(invalid(format!(
            "artifact is not a bounded regular file: {}",
            path.display()
        )));
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > ARTIFACT_BYTES {
        return Err(invalid("artifact exceeds v2 bound"));
    }
    Ok(bytes)
}

pub fn artifact_path(root: &Path, name: &str) -> Result<std::path::PathBuf, ExchangeError> {
    if !valid_artifact_name(name) {
        return Err(invalid("invalid artifact path"));
    }
    let relative = if let Some(name) = name.strip_prefix("root/") {
        Path::new("roots").join(name)
    } else {
        Path::new("transitions").join(name.trim_start_matches("transition/"))
    };
    if relative
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(invalid("artifact path traversal"));
    }
    Ok(root.join(relative))
}
