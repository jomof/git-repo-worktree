use crate::cli::ReportArgs;
use crate::discovery::discover_client;
use crate::error::{AppError, AppResult};
use crate::metrics::history_path;
use crate::registry::{load_all, registry_path};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub fn run(args: &ReportArgs, cwd: &Path, json_output: bool) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    let path = history_path(&client.root);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(AppError::io(format!("read {}", path.display()), error)),
    };
    let mut events = contents
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            args.session
                .as_ref()
                .map_or(true, |session| event["session"].as_str() == Some(session))
        })
        .collect::<Vec<_>>();
    if let Some(last) = args.last {
        let skip = events.len().saturating_sub(last);
        events.drain(..skip);
    }
    let trees = load_all(&registry_path(&client.root)).unwrap_or_default();
    let report = summarize(&events, &trees);
    let mut result = report.clone();
    result["history_path"] = json!(path);
    result["session"] = json!(args.session);
    if !json_output {
        print_human(&result);
    }
    Ok(result)
}

fn percentile(values: &mut [u64], fraction: f64) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let index = ((values.len() - 1) as f64 * fraction).round() as usize;
    Some(values[index])
}

fn latency(mut values: Vec<u64>) -> Value {
    json!({
        "count": values.len(),
        "p50_ms": percentile(&mut values, 0.5),
        "p90_ms": percentile(&mut values, 0.9),
        "max_ms": values.iter().max(),
    })
}

fn increment(counts: &mut BTreeMap<String, u64>, key: impl Into<String>) {
    *counts.entry(key.into()).or_default() += 1;
}

fn summarize(events: &[Value], trees: &[crate::model::TreeMeta]) -> Value {
    let mut by_command: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for event in events {
        let command = event["command"].as_str().unwrap_or("?").to_string();
        by_command.entry(command).or_default().push(event);
    }

    let commands = by_command
        .iter()
        .map(|(command, events)| {
            let mut errors = BTreeMap::new();
            for event in events.iter().filter(|event| event["exit_code"] != 0) {
                increment(&mut errors, event["error"].as_str().unwrap_or("?"));
            }
            let mut phases: BTreeMap<String, Vec<u64>> = BTreeMap::new();
            for event in events {
                if let Some(event_phases) = event["phases"].as_object() {
                    for (phase, ms) in event_phases {
                        if let Some(ms) = ms.as_u64() {
                            phases.entry(phase.clone()).or_default().push(ms);
                        }
                    }
                }
            }
            let phases = phases
                .into_iter()
                .map(|(phase, values)| (phase, latency(values)))
                .collect::<Map<_, _>>();
            (
                command.clone(),
                json!({
                    "latency": latency(events.iter().filter_map(|event| event["ms"].as_u64()).collect()),
                    "ok": events.iter().filter(|event| event["exit_code"] == 0).count(),
                    "errors": errors,
                    "phases": phases,
                    "via_git": events.iter().filter(|event| event["via_git"] == true).count(),
                }),
            )
        })
        .collect::<Map<_, _>>();

    let mut usage_errors: BTreeMap<String, u64> = BTreeMap::new();
    for event in events.iter().filter(|event| event["exit_code"] == 2) {
        let shape = event["args"]
            .as_array()
            .map(|args| {
                args.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        increment(
            &mut usage_errors,
            format!("{} [{}]", shape, event["error"].as_str().unwrap_or("?")),
        );
    }

    let adds = by_command.get("add").cloned().unwrap_or_default();
    let mut modes = BTreeMap::new();
    let mut strategies = BTreeMap::new();
    let mut per_project: BTreeMap<String, (Vec<u64>, String, Option<u64>)> = BTreeMap::new();
    let (mut pool_hits, mut pool_misses) = (0u64, 0u64);
    let mut failures = BTreeMap::new();
    let mut notes = BTreeMap::new();
    for event in &adds {
        for project in event["projects"].as_array().into_iter().flatten() {
            let mode = project["mode"].as_str().unwrap_or("?");
            increment(&mut modes, mode);
            increment(
                &mut strategies,
                project["strategy"].as_str().unwrap_or("none"),
            );
            if project["pooled"] == true {
                if project["strategy"] == "pool" {
                    pool_hits += 1;
                } else {
                    pool_misses += 1;
                }
            }
            if project["state"] == "failed" {
                increment(
                    &mut failures,
                    project["reason"]
                        .as_str()
                        .unwrap_or("?")
                        .chars()
                        .take(80)
                        .collect::<String>(),
                );
            }
            if let (Some(path), Some(ms)) = (project["path"].as_str(), project["ms"].as_u64()) {
                let entry = per_project
                    .entry(path.to_string())
                    .or_insert_with(|| (Vec::new(), String::new(), None));
                entry.0.push(ms);
                entry.1 = format!("{mode}/{}", project["strategy"].as_str().unwrap_or("-"));
                entry.2 = project["bytes"].as_u64();
            }
        }
        for note in event["notes"].as_array().into_iter().flatten() {
            let text = note["note"].as_str().unwrap_or("?");
            let kind = text.split(':').next().unwrap_or(text);
            increment(&mut notes, kind.chars().take(60).collect::<String>());
        }
    }
    let mut slowest = per_project
        .into_iter()
        .map(|(path, (mut values, how, bytes))| {
            let median = percentile(&mut values, 0.5).unwrap_or(0);
            json!({
                "project": path,
                "p50_ms": median,
                "max_ms": values.iter().max(),
                "samples": values.len(),
                "last": how,
                "bytes": bytes,
            })
        })
        .collect::<Vec<_>>();
    slowest.sort_by_key(|entry| std::cmp::Reverse(entry["p50_ms"].as_u64().unwrap_or(0)));
    slowest.truncate(15);

    let removes = by_command.get("remove").cloned().unwrap_or_default();
    let mut dirty_reasons = BTreeMap::new();
    for event in &removes {
        for project in event["dirty"].as_array().into_iter().flatten() {
            for reason in project["reasons"].as_array().into_iter().flatten() {
                increment(&mut dirty_reasons, reason.as_str().unwrap_or("?"));
            }
        }
    }

    let mut slow_inspections: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for event in events {
        for entry in event["slow_inspections"].as_array().into_iter().flatten() {
            if let (Some(project), Some(ms)) = (entry["project"].as_str(), entry["ms"].as_u64()) {
                slow_inspections
                    .entry(project.to_string())
                    .or_default()
                    .push(ms);
            }
        }
    }
    let mut slow_inspections = slow_inspections
        .into_iter()
        .map(|(project, values)| {
            let mut values = values;
            json!({"project": project, "p50_ms": percentile(&mut values, 0.5), "samples": values.len()})
        })
        .collect::<Vec<_>>();
    slow_inspections.sort_by_key(|entry| std::cmp::Reverse(entry["p50_ms"].as_u64().unwrap_or(0)));
    slow_inspections.truncate(10);

    let mut hooks: BTreeMap<String, (BTreeMap<String, u64>, Vec<u64>)> = BTreeMap::new();
    for event in events {
        for hook in event["hooks"].as_array().into_iter().flatten() {
            let entry = hooks
                .entry(hook["event"].as_str().unwrap_or("?").to_string())
                .or_default();
            increment(&mut entry.0, hook["status"].as_str().unwrap_or("?"));
            if let Some(ms) = hook["ms"].as_u64() {
                entry.1.push(ms);
            }
        }
    }
    let hooks = hooks
        .into_iter()
        .map(|(event, (statuses, durations))| {
            (
                event,
                json!({"statuses": statuses, "latency": latency(durations)}),
            )
        })
        .collect::<Map<_, _>>();

    let mut tree_states = BTreeMap::new();
    let mut tree_modes = BTreeMap::new();
    for tree in trees {
        increment(&mut tree_states, tree.state.clone());
        for project in tree
            .projects
            .iter()
            .filter(|project| project.state != "skipped")
        {
            increment(&mut tree_modes, project.mode.clone());
        }
    }

    json!({
        "events": events.len(),
        "first": events.first().map(|event| event["ts"].clone()),
        "last": events.last().map(|event| event["ts"].clone()),
        "environment": adds.last().map(|event| event["environment"].clone()),
        "commands": commands,
        "usage_errors": usage_errors,
        "add": {
            "modes": modes,
            "strategies": strategies,
            "pool": {"hits": pool_hits, "misses": pool_misses},
            "failures": failures,
            "notes": notes,
            "slowest_projects": slowest,
        },
        "remove": {
            "returned_to_pool": removes.iter().filter_map(|event| event["returned_to_pool"].as_u64()).sum::<u64>(),
            "dirty_reasons": dirty_reasons,
        },
        "slow_inspections": slow_inspections,
        "hooks": hooks,
        "live_trees": {
            "count": trees.len(),
            "states": tree_states,
            "modes": tree_modes,
        },
    })
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => "-".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn print_human(report: &Value) {
    println!(
        "{} event(s) from {} to {}",
        report["events"],
        report["first"].as_str().unwrap_or("-"),
        report["last"].as_str().unwrap_or("-")
    );
    if let Some(environment) = report["environment"].as_object() {
        let field = |name: &str| {
            environment
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        };
        println!(
            "environment: {} {} ({}), {} cpus, source fs {}, tree fs {}, overlay {}",
            field("os"),
            field("kernel"),
            field("git"),
            environment.get("cpus").cloned().unwrap_or(Value::Null),
            field("source_fs"),
            field("tree_fs"),
            environment
                .get("overlay_unavailable")
                .and_then(Value::as_str)
                .map_or("available".to_string(), |reason| format!(
                    "unavailable ({reason})"
                ))
        );
    }
    println!(
        "\n{:<12} {:>6} {:>4} {:>7} {:>7} {:>7}  errors",
        "command", "count", "ok", "p50_ms", "p90_ms", "max_ms"
    );
    for (command, summary) in report["commands"].as_object().into_iter().flatten() {
        let latency = &summary["latency"];
        let errors = summary["errors"]
            .as_object()
            .map(|errors| {
                errors
                    .iter()
                    .map(|(code, count)| format!("{code}x{count}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        println!(
            "{:<12} {:>6} {:>4} {:>7} {:>7} {:>7}  {}",
            command,
            text(&latency["count"]),
            text(&summary["ok"]),
            text(&latency["p50_ms"]),
            text(&latency["p90_ms"]),
            text(&latency["max_ms"]),
            errors
        );
    }
    let section = |title: &str, value: &Value| {
        if let Some(object) = value.as_object().filter(|object| !object.is_empty()) {
            println!("\n{title}:");
            for (key, count) in object {
                println!("  {count:>5}  {key}");
            }
        }
    };
    section("usage errors", &report["usage_errors"]);
    section("project modes (add)", &report["add"]["modes"]);
    section("strategies (add)", &report["add"]["strategies"]);
    section("population notes", &report["add"]["notes"]);
    section("project failures", &report["add"]["failures"]);
    section(
        "dirty refusals (remove)",
        &report["remove"]["dirty_reasons"],
    );
    for (event, summary) in report["hooks"].as_object().into_iter().flatten() {
        println!(
            "\nhook {event}: p50 {} ms, p90 {} ms, statuses {}",
            text(&summary["latency"]["p50_ms"]),
            text(&summary["latency"]["p90_ms"]),
            summary["statuses"]
        );
    }
    println!(
        "\npool: {} hit(s), {} miss(es); {} project(s) returned on remove",
        report["add"]["pool"]["hits"],
        report["add"]["pool"]["misses"],
        report["remove"]["returned_to_pool"]
    );
    if let Some(slowest) = report["add"]["slowest_projects"]
        .as_array()
        .filter(|list| !list.is_empty())
    {
        println!("\nslowest projects to populate (p50 ms, samples, mode/strategy):");
        for entry in slowest {
            println!(
                "  {:>8}  {:>3}  {:<28} {}",
                text(&entry["p50_ms"]),
                text(&entry["samples"]),
                text(&entry["last"]),
                text(&entry["project"])
            );
        }
    }
    println!(
        "\nlive trees: {}  (history: {})",
        report["live_trees"]["count"],
        report["history_path"].as_str().unwrap_or("")
    );
}
