use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() {
    let cli = match bumpkin::Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // --help/--version/errors: clap's Display impl prints appropriately
            e.exit();
        }
    };

    // Load the config before tracing is set up so `verbose = true` in the
    // config file selects the debug filter too.
    let config = match bumpkin::load_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };

    let verbose = cli.verbose || config.verbose.unwrap_or(false);
    let default_directive = if verbose {
        "bumpkin=debug"
    } else {
        "bumpkin=info"
    };
    let filter =
        EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new(default_directive));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_level(true)
        .without_time()
        .compact()
        .init();

    if let Err(err) = bumpkin::run(cli, config) {
        tracing::error!("{err}");
        std::process::exit(1);
    }
}
