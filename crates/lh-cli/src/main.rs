mod inference;
mod routing_eval;

use clap::{Parser, Subcommand};
use lh_core::{Config, Engine, Registry};
use lh_protocol::{
    RouteInput,
    json::{self, json},
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Local-first specialist routing")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Route a one-shot request and stream the specialist's answer.
    Run {
        #[arg(long, default_value = "auto")]
        model: String,
        #[arg(long)]
        image: Vec<String>,
        #[arg(long)]
        router: Option<String>,
        prompt: String,
    },
    Models {
        #[command(subcommand)]
        command: Models,
    },
    Eval {
        #[command(subcommand)]
        command: Eval,
    },
    Plugins {
        #[command(subcommand)]
        command: Plugins,
    },
    Route {
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        image: Vec<String>,
        #[arg(long)]
        audio: Vec<String>,
        #[arg(long)]
        router: Option<String>,
        prompt: String,
    },
}
#[derive(Subcommand)]
enum Plugins {
    List,
}
#[derive(Subcommand)]
enum Models {
    /// Download and load a specialist or router model, then shut down.
    Pull { id: String },
}
#[derive(Subcommand)]
enum Eval {
    /// Evaluate labelled JSONL routing cases without running specialists.
    Routing {
        #[arg(long, default_value = "evals/routing.jsonl")]
        file: PathBuf,
        #[arg(long)]
        router: Option<String>,
    },
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("lh: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let (config, directory) = Config::load(cli.config.as_deref())?;
    let registry = Registry::discover(&config, &directory)?;
    match cli.command {
        Commands::Run {
            model,
            image,
            router,
            prompt,
        } => {
            let mut engine = Engine::new(registry, config);
            let result = inference::run(&mut engine, model, image, router.as_deref(), prompt).await;
            engine.shutdown().await;
            result?;
        }
        Commands::Models {
            command: Models::Pull { id },
        } => {
            let mut engine = Engine::new(registry, config);
            let result = inference::pull(&mut engine, &id).await;
            engine.shutdown().await;
            println!("{}", json::to_string_pretty(&result?)?);
        }
        Commands::Eval {
            command: Eval::Routing { file, router },
        } => {
            let mut engine = Engine::new(registry, config);
            let result = routing_eval::evaluate(&mut engine, &file, router.as_deref()).await;
            engine.shutdown().await;
            let report = result?;
            println!("{}", json::to_string_pretty(&report)?);
            if report["error_count"].as_u64().unwrap_or(0) != 0 {
                return Err("routing evaluation encountered errors (see JSON report)".into());
            }
        }
        Commands::Plugins {
            command: Plugins::List,
        } => {
            let plugins: Vec<_> = registry.plugins.values().map(|entry| {
                let plugin = &entry.manifest.plugin;
                let enabled = config.plugins.get(&plugin.id).and_then(|c| c.enabled).unwrap_or(plugin.enabled);
                json!({"id":plugin.id,"version":plugin.version,"provides":plugin.provides,"capabilities":plugin.capabilities,"capabilities_source":"manifest","enabled":enabled,"manifest":entry.path,"status":if !enabled {"disabled"} else if entry.manifest.run.is_some() {"not-started"} else {"declarative"}})
            }).collect();
            println!(
                "{}",
                json::to_string_pretty(
                    &json!({"plugins":plugins,"specialists":registry.specialists.keys().collect::<Vec<_>>(),"routers":registry.routers.keys().collect::<Vec<_>>()})
                )?
            );
        }
        Commands::Route {
            explain,
            image,
            audio,
            router,
            prompt,
        } => {
            let mut engine = Engine::new(registry, config);
            let result = engine
                .route(
                    &RouteInput {
                        prompt,
                        images: image,
                        audio,
                        specialist: None,
                    },
                    router.as_deref(),
                )
                .await;
            engine.shutdown().await;
            let result = result?;
            println!(
                "{}",
                if explain {
                    json::to_string_pretty(&result)?
                } else {
                    json::to_string(&result)?
                }
            );
        }
    }
    Ok(())
}
