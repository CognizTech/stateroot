//! `stateroot todo` — federated harness todo lists.

use stateroot_core::todo_federation::{self, TodoItem, TodoRecord};

use super::Ctx;

/// Run `stateroot todo list [--harness H]`.
pub fn list(ctx: &Ctx, harness: Option<&str>) -> anyhow::Result<()> {
    ctx.require_project()?;
    let rows = todo_federation::current_lists(&ctx.cwd, harness);
    if rows.is_empty() {
        println!("no federated todos");
        return Ok(());
    }
    for record in rows {
        println!("{}", format_record(&record));
        for item in &record.items {
            println!("  {} {}", marker(item), item.content);
        }
    }
    Ok(())
}

fn format_record(record: &TodoRecord) -> String {
    let done = record
        .items
        .iter()
        .filter(|item| item.status == "completed")
        .count();
    let total = record.items.len();
    let bind = if let Some(plan_id) = &record.plan_id {
        format!("plan-bound {plan_id}")
    } else {
        "standalone".to_string()
    };
    format!(
        "{} · {} · {bind} · todos {done}/{total}",
        record.harness, record.session_id
    )
}

fn marker(item: &TodoItem) -> &'static str {
    match item.status.as_str() {
        "completed" => "[x]",
        "in_progress" => "[~]",
        _ => "[ ]",
    }
}
