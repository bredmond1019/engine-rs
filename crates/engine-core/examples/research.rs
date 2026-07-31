use std::collections::HashMap;
use std::env;

use engine_contract::TaskContext;
use engine_core::workflow::Workflow;
use engine_core::workflows::research_agent::graph;
use serde_json::json;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  cargo run --example research -- company <company_name> [company_url]");
        eprintln!("  cargo run --example research -- prospecting <vertical> [topic]");
        std::process::exit(1);
    }

    let mode = &args[1];
    let event = match mode.as_str() {
        "company" => {
            if args.len() < 3 {
                eprintln!("Error: company mode requires <company_name>");
                std::process::exit(1);
            }
            json!({
                "mode": "company",
                "company_name": args[2],
                "company_url": args.get(3).cloned(),
            })
        }
        "prospecting" => {
            if args.len() < 3 {
                eprintln!("Error: prospecting mode requires <vertical>");
                std::process::exit(1);
            }
            json!({
                "mode": "prospecting",
                "vertical": args[2],
                "topic": args.get(3).cloned(),
            })
        }
        _ => {
            eprintln!("Unknown mode: {}. Use 'company' or 'prospecting'", mode);
            std::process::exit(1);
        }
    };

    println!("Initializing RESEARCH_AGENT...");
    let registry = graph::registry();
    let schema = graph::schema();
    let workflow = Workflow::new_validated(registry, schema).unwrap();

    let mut ctx = TaskContext {
        event: event.clone(),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };

    // Set up a scratch worktree for state files
    let worktree = std::env::temp_dir().join(format!("research-run-{}", std::process::id()));
    // Guarantee-empty: see engine-core's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional (this is a scratch worktree, not a test, but the same latent
    // hazard applies).
    std::fs::remove_dir_all(&worktree).ok();
    std::fs::create_dir_all(&worktree).unwrap();
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );

    println!("Running in {} mode. This may take 30-60 seconds...", mode);

    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .unwrap();

    println!("\n====================== RESULT ======================");
    if mode == "company" {
        if let Some(result) = final_ctx.nodes.get("CompanyResearchNode") {
            println!("{}", serde_json::to_string_pretty(result).unwrap());
        }
    } else if mode == "prospecting" {
        if let Some(result) = final_ctx.nodes.get("ProspectingResearchNode") {
            println!("{}", serde_json::to_string_pretty(result).unwrap());
        }
    }
    println!("=====================================================\n");
}
