#![deny(warnings)]

mod app;
mod cli;
mod config;
mod db;
mod filter_engine;
mod filter_lists;
mod mpris;
mod pages;
mod tabs;
mod thumbnails;
mod url_bar;
mod user_content;

use clap::Parser as _;
use cosmic_config::CosmicConfigEntry as _;
use libcosmic::app::Settings;
use libcosmic::config::CosmicTk;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Note: do not initialize tracing_subscriber here. Servo sets its own global
    // log::logger early in app startup; a second set_logger call panics.

    // Bad arguments, --help and --version all exit here, before the engine starts.
    let options = cli::Cli::parse();

    // Directory overrides must land before anything reads the defaults.
    if let Some(dir) = options.profile_dir.clone() {
        db::set_profile_dir(dir);
    }
    if let Some(dir) = options.userscripts_dir.clone() {
        user_content::set_scripts_dir(dir);
    }

    // Read the COSMIC toolkit config to pick up the system interface font family.
    // This must happen before the app starts; live font-size updates are handled in Fas 12.
    let tk = CosmicTk::config()
        .ok()
        .map(|c| CosmicTk::get_entry(&c).unwrap_or_else(|(_, tk)| tk))
        .unwrap_or_default();

    let font: libcosmic::iced::Font = tk.interface_font.into();

    let settings = Settings::default()
        .size((1280.0, 800.0).into())
        .default_font(font);

    libcosmic::app::run::<app::App>(settings, options)?;
    Ok(())
}
