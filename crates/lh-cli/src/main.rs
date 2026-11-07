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
        Commands::Plugins {
            command: Plugins::List,
        } => {
            let plugins: Vec<_> = registry.plugins.values().map(|entry| json!({"id":entry.manifest.plugin.id,"version":entry.manifest.plugin.version,"provides":entry.manifest.plugin.provides,"manifest":entry.path,"status":if entry.manifest.run.is_some() {"not-started"} else {"declarative"}})).collect();
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
