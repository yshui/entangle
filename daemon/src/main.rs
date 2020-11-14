#![feature(option_unwrap_none, never_type, exhaustive_patterns, array_value_iter)]
use ::anyhow::Result;
use ::std::path::{Path, PathBuf};

use ::argh::FromArgs;
use ::async_std::net::IpAddr;

/// Entangled subcommands
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum EntangledSubcommands {
    Server(EntangledServerOpts),
    Client(EntangledClientOpts),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "server")]
/// Start an entangle server
struct EntangledServerOpts {}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "client")]
/// Connect to an entangle server
struct EntangledClientOpts {
    #[argh(option, short = 's')]
    /// server address, must be one of the peers in your config file
    server: IpAddr,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Entangled
struct EntangledOpts {
    #[argh(
        option,
        short = 'c',
        default = "Path::new(\"/etc/entangle.conf\").into()"
    )]
    /// path to your configuration file. (default: /etc/entangle.conf)
    config: PathBuf,
    #[argh(subcommand)]
    subcommand: EntangledSubcommands,
}

mod client;
mod proto;
mod server;
mod uinput;
mod evdev;

fn main() -> Result<()> {
    ::env_logger::init();
    let opts: EntangledOpts = argh::from_env();
    let cfg: ::config::Config = ::toml::from_str(&::std::fs::read_to_string(&opts.config)?)?;
    use EntangledSubcommands::*;
    match opts.subcommand {
        Server(server) => ::async_std::task::block_on(server::run(cfg, server))?,
        Client(client) => ::async_std::task::block_on(client::run(cfg, client))?,
    }
}
