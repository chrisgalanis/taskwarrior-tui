use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use linear_sync::{config::Config, LinearClient, LinearTeam};
use serde_json::Value;
use std::io::{self, BufRead, Write};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "linear-sync", about = "Sync taskwarrior tasks with Linear issues")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Interactive first-time setup: configures token, team, and assignee
    Setup,
    /// Print your user ID, team IDs, and workflow state IDs (needs a read-scope token)
    Ids {
        /// Linear API token with at least 'read' scope
        #[arg(long, env = "LINEAR_API_TOKEN")]
        token: Option<String>,
    },
    /// Taskwarrior on-add hook: pass task through unchanged (Linear creation is now explicit)
    OnAdd,
    /// Taskwarrior on-modify hook: sync status changes and new annotations to Linear
    OnModify,
    /// Install hook scripts into ~/.task/hooks/ and add UDAs to ~/.taskrc
    InstallHooks,
    /// Create a Linear issue for an existing task and store the IDs back on the task
    PushTask {
        /// UUID of the taskwarrior task
        uuid: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Setup => setup().await,
        Cmd::Ids { token } => print_ids(token).await,
        Cmd::OnAdd => on_add().await,
        Cmd::OnModify => on_modify().await,
        Cmd::InstallHooks => install_hooks(),
        Cmd::PushTask { uuid } => push_task(&uuid).await,
    }
}

// ── setup ────────────────────────────────────────────────────────────────────

fn prompt(label: &str, default: Option<&str>) -> Result<String> {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match default {
        Some(d) => write!(err, "{} [{}]: ", label, d)?,
        None => write!(err, "{}: ", label)?,
    }
    err.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let value = line.trim().to_string();
    if value.is_empty() {
        default
            .map(|d| d.to_string())
            .context(format!("{} is required", label))
    } else {
        Ok(value)
    }
}

async fn setup() -> Result<()> {
    eprintln!("=== linear-sync setup ===\n");
    eprintln!("Go to Linear → Settings → API → Personal API Keys and create a token.");
    eprintln!("Minimum required scopes: Create Issues + Create Comments.\n");

    let token = prompt("API token", None)?;
    let client = LinearClient::new(&token);

    // Try auto-discovery with read scope; fall back to manual entry if not available.
    let (assignee_id, team_id, done_state_id, in_progress_state_id) = match client.get_viewer().await {
        Ok(viewer) => {
            eprintln!("  ✓ Authenticated as {} ({})\n", viewer.name, viewer.email);
            auto_discover(&client, &viewer.email).await?
        }
        Err(_) => {
            // Token doesn't have read scope — that's fine, just ask for IDs directly.
            eprintln!("  ✓ Token accepted (no read scope — entering IDs manually)\n");
            eprintln!("  Tip: find your IDs by running this query in the Linear GraphQL playground");
            eprintln!("  (linear.app → Help → API → Playground):\n");
            eprintln!("    {{ viewer {{ id }} teams {{ nodes {{ id name states {{ nodes {{ id name type }} }} }} }} }}\n");
            manual_discover()?
        }
    };

    let config = Config { api_token: token, team_id, assignee_id, done_state_id, in_progress_state_id };
    config.save()?;
    eprintln!("\n  ✓ Config saved to {}", Config::path().display());
    eprintln!("    Run `linear-sync install-hooks` to wire up taskwarrior.");
    Ok(())
}

async fn auto_discover(client: &LinearClient, viewer_email: &str) -> Result<(String, String, String, Option<String>)> {
    let email = prompt("Assignee email", Some(viewer_email))?;
    let user = client
        .find_user_by_email(&email)
        .await?
        .with_context(|| format!("No Linear user found with email '{}'", email))?;
    eprintln!("  Assignee: {} ({})\n", user.name, user.id);

    let teams = client.get_teams().await?;
    let team = pick_team(&teams)?;

    let done_state = team
        .states
        .nodes
        .iter()
        .find(|s| s.kind == "completed")
        .with_context(|| format!("No completed state in team '{}'", team.name))?;
    eprintln!("  Done state: {} ({})", done_state.name, done_state.id);

    let in_progress_state = team.states.nodes.iter().find(|s| s.kind == "started");
    if let Some(s) = &in_progress_state {
        eprintln!("  In-progress state: {} ({})\n", s.name, s.id);
    } else {
        eprintln!("  No 'started'-type state found — in-progress sync disabled\n");
    }

    Ok((user.id, team.id.clone(), done_state.id.clone(), in_progress_state.map(|s| s.id.clone())))
}

fn manual_discover() -> Result<(String, String, String, Option<String>)> {
    let assignee_id = prompt("Your Linear user ID  (viewer.id from the query above)", None)?;
    let team_id     = prompt("Team ID              (teams.nodes[n].id)", None)?;
    let done_state_id = prompt("Done workflow state ID  (the node where type==\"completed\")", None)?;
    eprintln!("  (optional) In-progress workflow state ID — leave blank to skip");
    let in_progress_state_id = prompt("In-progress workflow state ID", Some(""))
        .ok()
        .and_then(|s| if s.is_empty() { None } else { Some(s) });
    Ok((assignee_id, team_id, done_state_id, in_progress_state_id))
}

fn pick_team<'a>(teams: &'a [LinearTeam]) -> Result<&'a LinearTeam> {
    if teams.len() == 1 {
        eprintln!("  Team: {} ({})\n", teams[0].name, teams[0].id);
        return Ok(&teams[0]);
    }
    eprintln!("  Available teams:");
    for (i, t) in teams.iter().enumerate() {
        eprintln!("    {}. {}", i + 1, t.name);
    }
    eprint!("  Select team [1-{}]: ", teams.len());
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let idx = line
        .trim()
        .parse::<usize>()
        .context("Expected a number")?
        .saturating_sub(1);
    teams.get(idx).context("Index out of range")
}

// ── ids ───────────────────────────────────────────────────────────────────────

async fn print_ids(token_arg: Option<String>) -> Result<()> {
    let token = match token_arg {
        Some(t) => t,
        None => prompt("Read-scope API token", None)?,
    };

    let client = LinearClient::new(&token);

    let viewer = client
        .get_viewer()
        .await
        .context("Authentication failed — this command needs a token with 'read' scope")?;

    println!("\n── You ──────────────────────────────────");
    println!("  user id : {}", viewer.id);
    println!("  name    : {}", viewer.name);
    println!("  email   : {}", viewer.email);

    let teams = client.get_teams().await?;
    for team in &teams {
        println!("\n── Team: {} ─────────────────────────────", team.name);
        println!("  team id : {}", team.id);
        println!("  workflow states:");
        for state in &team.states.nodes {
            println!(
                "    {:.<40} id: {}  type: {}",
                format!("{} ", state.name),
                state.id,
                state.kind
            );
        }
    }
    println!();
    Ok(())
}

// ── on-add hook ──────────────────────────────────────────────────────────────

async fn on_add() -> Result<()> {
    // Pass task through unchanged. Linear issues are created explicitly via
    // `linear-sync push-task <uuid>` (triggered by the TUI's ctrl+l add flow).
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    print!("{}", line.trim());
    Ok(())
}

// ── push-task ─────────────────────────────────────────────────────────────────

async fn push_task(uuid: &str) -> Result<()> {
    // Validate UUID format early for a clear error message.
    uuid.parse::<Uuid>().context("Invalid UUID")?;

    let output = std::process::Command::new("task")
        .arg(uuid)
        .arg("export")
        .output()
        .context("Failed to run `task export`")?;

    anyhow::ensure!(
        output.status.success(),
        "task export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json_str = String::from_utf8_lossy(&output.stdout);
    let tasks: Vec<Value> =
        serde_json::from_str(&json_str).context("Failed to parse task export JSON")?;
    let mut task = tasks.into_iter().next().context("No task found with that UUID")?;

    if task["linearid"].as_str().is_some() {
        eprintln!("linear-sync: task already has a Linear issue, skipping");
        return Ok(());
    }

    let config = Config::load()?;

    let title = task["description"].as_str().unwrap_or("Untitled task").to_string();
    let description = task["lineardescription"].as_str();

    let client = LinearClient::new(&config.api_token);
    let issue = client
        .create_issue(&title, description.as_deref(), &config.team_id, &config.assignee_id)
        .await?;

    eprintln!("linear-sync: created {} — {}", issue.identifier, issue.url);

    if let Some(obj) = task.as_object_mut() {
        obj.insert("linearid".into(), Value::String(issue.id));
        obj.insert("linearurl".into(), Value::String(issue.url.clone()));
        obj.insert("linearidentifier".into(), Value::String(issue.identifier));
    }

    let modified_json = serde_json::to_string(&task)?;

    let mut import_cmd = std::process::Command::new("task")
        .arg("import")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn `task import`")?;

    if let Some(mut stdin) = import_cmd.stdin.take() {
        stdin.write_all(modified_json.as_bytes())?;
    }

    let status = import_cmd.wait().context("Failed to wait for `task import`")?;
    anyhow::ensure!(status.success(), "task import failed");

    eprintln!("linear-sync: task updated with Linear IDs");
    Ok(())
}

// ── on-modify hook ───────────────────────────────────────────────────────────

async fn on_modify() -> Result<()> {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    let old_line = lines
        .next()
        .context("Missing original task JSON")??
        .trim()
        .to_string();
    let new_line = lines
        .next()
        .context("Missing modified task JSON")??
        .trim()
        .to_string();

    let old_task: Value =
        serde_json::from_str(&old_line).context("Failed to parse original task")?;
    let mut new_task: Value =
        serde_json::from_str(&new_line).context("Failed to parse modified task")?;

    let linear_id = match new_task["linearid"].as_str() {
        Some(id) => id.to_string(),
        None => {
            // Task has no Linear issue — nothing to sync.
            println!("{}", new_line);
            return Ok(());
        }
    };

    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("linear-sync: skipping on-modify ({})", e);
            println!("{}", new_line);
            return Ok(());
        }
    };

    let client = LinearClient::new(&config.api_token);

    // 1. Status: pending → completed
    let old_status = old_task["status"].as_str().unwrap_or("");
    let new_status = new_task["status"].as_str().unwrap_or("");
    if old_status != "completed" && new_status == "completed" {
        match client.set_issue_state(&linear_id, &config.done_state_id).await {
            Ok(()) => eprintln!("linear-sync: marked Linear issue as completed"),
            Err(e) => eprintln!("linear-sync: failed to complete Linear issue: {:#}", e),
        }
    }

    // 1b. Task started (Triage → In Progress)
    let was_active = old_task["start"].as_str().is_some();
    let is_active = new_task["start"].as_str().is_some();
    if !was_active && is_active {
        if let Some(ref state_id) = config.in_progress_state_id {
            match client.set_issue_state(&linear_id, state_id).await {
                Ok(()) => eprintln!("linear-sync: moved Linear issue to in-progress"),
                Err(e) => eprintln!("linear-sync: failed to set in-progress state: {:#}", e),
            }
        }
    }

    // 2. Description changed → update issue title
    let old_desc = old_task["description"].as_str().unwrap_or("");
    let new_desc = new_task["description"].as_str().unwrap_or("");
    if old_desc != new_desc && !new_desc.is_empty() {
        match client.update_issue(&linear_id, Some(new_desc), None).await {
            Ok(()) => eprintln!("linear-sync: updated Linear issue title"),
            Err(e) => eprintln!("linear-sync: failed to update Linear issue title: {:#}", e),
        }
    }

    // 3. lineardescription changed → update Linear issue body
    let old_linear_desc = old_task["lineardescription"].as_str().unwrap_or("");
    let new_linear_desc = new_task["lineardescription"].as_str().unwrap_or("");
    if old_linear_desc != new_linear_desc && !new_linear_desc.is_empty() {
        match client.update_issue(&linear_id, None, Some(new_linear_desc)).await {
            Ok(()) => eprintln!("linear-sync: updated Linear issue description"),
            Err(e) => eprintln!("linear-sync: failed to update Linear issue description: {:#}", e),
        }
    }

    // 4. linearcomment set → post comment to Linear, then clear the field
    let new_comment = new_task["linearcomment"].as_str().unwrap_or("").to_string();
    if !new_comment.is_empty() {
        match client.create_comment(&linear_id, &new_comment).await {
            Ok(comment) => eprintln!("linear-sync: added comment {} to Linear issue", comment.id),
            Err(e) => eprintln!("linear-sync: failed to create Linear comment: {:#}", e),
        }
        if let Some(obj) = new_task.as_object_mut() {
            obj.remove("linearcomment");
        }
    }

    // 5. New annotations → create comments for each new entry
    let old_annotations = annotations_set(&old_task);
    let new_annotations = annotations_set(&new_task);
    for (entry_key, body) in &new_annotations {
        if !old_annotations.contains_key(entry_key.as_str()) {
            match client.create_comment(&linear_id, body).await {
                Ok(comment) => eprintln!(
                    "linear-sync: added comment {} to Linear issue",
                    comment.id
                ),
                Err(e) => eprintln!("linear-sync: failed to create Linear comment: {:#}", e),
            }
        }
    }

    println!("{}", serde_json::to_string(&new_task)?);
    Ok(())
}

fn annotations_set(task: &Value) -> std::collections::HashMap<String, String> {
    task["annotations"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let entry = a["entry"].as_str()?;
                    let desc = a["description"].as_str()?;
                    Some((entry.to_string(), desc.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── install-hooks ─────────────────────────────────────────────────────────────

fn install_hooks() -> Result<()> {
    let hooks_dir = dirs::home_dir()
        .context("Cannot determine home directory")?
        .join(".task")
        .join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let binary = std::env::current_exe().context("Cannot determine binary path")?;
    let binary_str = binary
        .to_str()
        .context("Non-UTF-8 binary path")?
        .to_string();

    for (hook_name, subcommand) in [("on-add-linear", "on-add"), ("on-modify-linear", "on-modify")] {
        let hook_path = hooks_dir.join(hook_name);
        let script = format!("#!/bin/sh\nexec \"{binary_str}\" {subcommand}\n");
        std::fs::write(&hook_path, &script)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
        }
        eprintln!("Installed: {}", hook_path.display());
    }

    // Append UDA definitions to ~/.taskrc if not already present
    let taskrc = dirs::home_dir()
        .context("Cannot determine home directory")?
        .join(".taskrc");

    let existing = std::fs::read_to_string(&taskrc).unwrap_or_default();
    let uda_lines = [
        "uda.linearid.type=string",
        "uda.linearid.label=Linear Issue ID",
        "uda.linearurl.type=string",
        "uda.linearurl.label=Linear Issue URL",
        "uda.linearidentifier.type=string",
        "uda.linearidentifier.label=Linear Identifier",
        "uda.lineardescription.type=string",
        "uda.lineardescription.label=Linear Description",
        "uda.linearcomment.type=string",
        "uda.linearcomment.label=Linear Comment",
    ];

    let missing: Vec<&str> = uda_lines
        .iter()
        .copied()
        .filter(|l| !existing.contains(l))
        .collect();

    if !missing.is_empty() {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&taskrc)
            .with_context(|| format!("Cannot open {}", taskrc.display()))?;
        writeln!(file, "\n# linear-sync UDAs")?;
        for line in &missing {
            writeln!(file, "{}", line)?;
        }
        eprintln!("Added UDA definitions to {}", taskrc.display());
    }

    eprintln!("\nDone! Tasks created from now on will be synced to Linear.");
    Ok(())
}
