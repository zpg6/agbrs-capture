//! GBA GIF Capture Tool
//!
//! Captures frames from mGBA windows and creates GIFs automatically
//! for each binary in an agbrs project.

use anyhow::Result;
use clap::Parser;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use gif::{Encoder, Frame, Repeat};
use image::{ImageBuffer, RgbImage, RgbaImage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::time::sleep;
use xcap::Window;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(about = "Captures frames from mGBA windows and creates GIFs for agbrs binaries")]
struct Args {
    /// Path to the agbrs project directory (defaults to current directory)
    #[arg(
        help = "Directory containing Cargo.toml and src/bin/ or src/main.rs with agbrs binaries (defaults to current directory)"
    )]
    project_dir: Option<PathBuf>,

    /// Frames per second for the output GIF
    #[arg(long, default_value_t = 10.0)]
    #[arg(help = "GIF framerate (frames per second)")]
    fps: f32,

    /// Duration of the GIF in seconds
    #[arg(long, default_value_t = 3.0)]
    #[arg(help = "GIF duration in seconds")]
    duration: f32,

    /// Input sequence to execute before capture starts
    #[arg(long)]
    #[arg(
        help = "Input sequence before capture (e.g., 'A:500,wait:1000,B' for A held 500ms, wait 1s, B quick press)"
    )]
    before_capture: Option<String>,

    /// Input sequence to execute during capture
    #[arg(long)]
    #[arg(
        help = "Input sequence during capture (e.g., 'right:100,wait:500,right:100' for directional inputs)"
    )]
    during_capture: Option<String>,
}

/// Input actions that can be performed on the mGBA window
#[derive(Debug, Clone)]
enum InputAction {
    /// Press and release a key (optional hold duration in milliseconds)
    Press { key: Key, duration_ms: Option<u64> },
    /// Press a key down (manual release required)
    KeyDown { key: Key },
    /// Release a previously pressed key
    KeyUp { key: Key },
    /// Wait for a specified duration
    Wait { duration_ms: u64 },
}

/// GBA controller button mappings to keyboard keys
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GbaKeyMappings {
    /// A button (default: x)
    #[serde(default = "default_button_a")]
    pub a: String,
    /// B button (default: z)  
    #[serde(default = "default_button_b")]
    pub b: String,
    /// Select button (default: backspace)
    #[serde(default = "default_select")]
    pub select: String,
    /// Start button (default: enter)
    #[serde(default = "default_start")]
    pub start: String,
    /// D-pad Right (default: right)
    #[serde(default = "default_dpad_right")]
    pub right: String,
    /// D-pad Left (default: left)
    #[serde(default = "default_dpad_left")]
    pub left: String,
    /// D-pad Up (default: up)
    #[serde(default = "default_dpad_up")]
    pub up: String,
    /// D-pad Down (default: down)
    #[serde(default = "default_dpad_down")]
    pub down: String,
    /// Right shoulder button (default: s)
    #[serde(default = "default_button_r")]
    pub r_shoulder: String,
    /// Left shoulder button (default: a)
    #[serde(default = "default_button_l")]
    pub l_shoulder: String,
}

// Default key mapping functions using your specified defaults
fn default_button_a() -> String {
    "x".to_string()
}
fn default_button_b() -> String {
    "z".to_string()
}
fn default_select() -> String {
    "backspace".to_string()
}
fn default_start() -> String {
    "enter".to_string()
}
fn default_dpad_right() -> String {
    "right".to_string()
}
fn default_dpad_left() -> String {
    "left".to_string()
}
fn default_dpad_up() -> String {
    "up".to_string()
}
fn default_dpad_down() -> String {
    "down".to_string()
}
fn default_button_r() -> String {
    "s".to_string()
}
fn default_button_l() -> String {
    "a".to_string()
}

impl Default for GbaKeyMappings {
    fn default() -> Self {
        Self {
            a: default_button_a(),
            b: default_button_b(),
            select: default_select(),
            start: default_start(),
            right: default_dpad_right(),
            left: default_dpad_left(),
            up: default_dpad_up(),
            down: default_dpad_down(),
            r_shoulder: default_button_r(),
            l_shoulder: default_button_l(),
        }
    }
}

/// Configuration for a single binary's input sequences
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BinaryConfig {
    /// Input sequence to execute before capture starts
    #[serde(skip_serializing_if = "Option::is_none")]
    before_capture: Option<String>,
    /// Input sequence to execute during capture
    #[serde(skip_serializing_if = "Option::is_none")]
    during_capture: Option<String>,
    /// Custom GBA key mappings for this binary
    #[serde(skip_serializing_if = "Option::is_none")]
    key_mappings: Option<GbaKeyMappings>,
}

/// Settings section of configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigSettings {
    /// Global GBA key mappings
    #[serde(skip_serializing_if = "Option::is_none")]
    key_mappings: Option<GbaKeyMappings>,
    /// Default configuration applied to all binaries (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<BinaryConfig>,
}

/// Main configuration structure for capture.json
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CaptureConfig {
    /// Global settings (key mappings, defaults, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<ConfigSettings>,
    /// Per-binary configurations
    #[serde(skip_serializing_if = "Option::is_none")]
    binaries: Option<HashMap<String, BinaryConfig>>,
}

/// Loads capture configuration from capture.json file
fn load_capture_config(project_dir: &Path) -> Result<Option<CaptureConfig>> {
    let config_path = project_dir.join("capture.json");

    if !config_path.exists() {
        return Ok(None);
    }

    let config_content = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to read capture.json: {}", e))?;

    let config: CaptureConfig = serde_json::from_str(&config_content)
        .map_err(|e| anyhow::anyhow!("Failed to parse capture.json: {}", e))?;

    Ok(Some(config))
}

/// Gets the input sequences for a specific binary from config or CLI args
fn get_binary_input_sequences(
    binary_name: &str,
    config: &Option<CaptureConfig>,
    cli_before: &Option<String>,
    cli_during: &Option<String>,
) -> (Option<String>, Option<String>) {
    // CLI args take precedence over config file
    if cli_before.is_some() || cli_during.is_some() {
        return (cli_before.clone(), cli_during.clone());
    }

    // Try to get from config file
    if let Some(config) = config {
        // Check for binary-specific config first
        if let Some(binaries) = &config.binaries {
            if let Some(binary_config) = binaries.get(binary_name) {
                return (
                    binary_config.before_capture.clone(),
                    binary_config.during_capture.clone(),
                );
            }
        }

        // Fall back to default config in settings
        if let Some(settings) = &config.settings {
            if let Some(default_config) = &settings.default {
                return (
                    default_config.before_capture.clone(),
                    default_config.during_capture.clone(),
                );
            }
        }
    }

    // No config found
    (None, None)
}

/// Gets the effective key mappings for a binary (binary > global > default)
fn get_effective_key_mappings(binary_name: &str, config: &Option<CaptureConfig>) -> GbaKeyMappings {
    if let Some(config) = config {
        // Check for binary-specific key mappings first
        if let Some(binaries) = &config.binaries {
            if let Some(binary_config) = binaries.get(binary_name) {
                if let Some(ref mappings) = binary_config.key_mappings {
                    return mappings.clone();
                }
            }
        }

        // Fall back to global key mappings in settings
        if let Some(settings) = &config.settings {
            if let Some(ref mappings) = settings.key_mappings {
                return mappings.clone();
            }
        }
    }

    // Use default mappings
    GbaKeyMappings::default()
}

/// Parses a string like "A:500,wait:1000,B" into a sequence of input actions
fn parse_input_sequence(input: &str, key_mappings: &GbaKeyMappings) -> Result<Vec<InputAction>> {
    let mut actions = Vec::new();

    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        if part.starts_with("wait:") {
            let duration_str = part.strip_prefix("wait:").unwrap();
            let duration_ms = duration_str
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("Invalid wait duration: {}", duration_str))?;
            actions.push(InputAction::Wait { duration_ms });
        } else if part.contains(':') {
            // Key with duration (hold)
            let mut split = part.split(':');
            let key_str = split.next().unwrap();
            let duration_str = split
                .next()
                .ok_or_else(|| anyhow::anyhow!("Invalid key:duration format: {}", part))?;
            let duration_ms = duration_str
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("Invalid duration: {}", duration_str))?;
            let key = parse_key(key_str, key_mappings)?;
            actions.push(InputAction::Press {
                key,
                duration_ms: Some(duration_ms),
            });
        } else {
            // Simple key press
            let key = parse_key(part, key_mappings)?;
            actions.push(InputAction::Press {
                key,
                duration_ms: None,
            });
        }
    }

    Ok(actions)
}

/// Parses a raw keyboard key string into an enigo Key (no GBA mappings)
fn parse_raw_key(key_str: &str) -> Result<Key> {
    match key_str.to_lowercase().as_str() {
        // Letters
        "a" => Ok(Key::Unicode('a')),
        "b" => Ok(Key::Unicode('b')),
        "c" => Ok(Key::Unicode('c')),
        "d" => Ok(Key::Unicode('d')),
        "e" => Ok(Key::Unicode('e')),
        "f" => Ok(Key::Unicode('f')),
        "g" => Ok(Key::Unicode('g')),
        "h" => Ok(Key::Unicode('h')),
        "i" => Ok(Key::Unicode('i')),
        "j" => Ok(Key::Unicode('j')),
        "k" => Ok(Key::Unicode('k')),
        "l" => Ok(Key::Unicode('l')),
        "m" => Ok(Key::Unicode('m')),
        "n" => Ok(Key::Unicode('n')),
        "o" => Ok(Key::Unicode('o')),
        "p" => Ok(Key::Unicode('p')),
        "q" => Ok(Key::Unicode('q')),
        "r" => Ok(Key::Unicode('r')),
        "s" => Ok(Key::Unicode('s')),
        "t" => Ok(Key::Unicode('t')),
        "u" => Ok(Key::Unicode('u')),
        "v" => Ok(Key::Unicode('v')),
        "w" => Ok(Key::Unicode('w')),
        "x" => Ok(Key::Unicode('x')),
        "y" => Ok(Key::Unicode('y')),
        "z" => Ok(Key::Unicode('z')),

        // Arrow keys (common for GBA games)
        "up" | "arrow_up" => Ok(Key::UpArrow),
        "down" | "arrow_down" => Ok(Key::DownArrow),
        "left" | "arrow_left" => Ok(Key::LeftArrow),
        "right" | "arrow_right" => Ok(Key::RightArrow),

        // Special keys
        "space" => Ok(Key::Unicode(' ')),
        "enter" | "return" => Ok(Key::Return),
        "tab" => Ok(Key::Tab),
        "escape" | "esc" => Ok(Key::Escape),
        "shift" => Ok(Key::Shift),
        "ctrl" | "control" => Ok(Key::Control),
        "alt" => Ok(Key::Alt),
        "backspace" => Ok(Key::Backspace),

        // Numbers
        "0" => Ok(Key::Unicode('0')),
        "1" => Ok(Key::Unicode('1')),
        "2" => Ok(Key::Unicode('2')),
        "3" => Ok(Key::Unicode('3')),
        "4" => Ok(Key::Unicode('4')),
        "5" => Ok(Key::Unicode('5')),
        "6" => Ok(Key::Unicode('6')),
        "7" => Ok(Key::Unicode('7')),
        "8" => Ok(Key::Unicode('8')),
        "9" => Ok(Key::Unicode('9')),

        _ => Err(anyhow::anyhow!("Unsupported key: {}", key_str)),
    }
}

/// Parses a string into an enigo Key, supporting GBA controller names
fn parse_key(key_str: &str, key_mappings: &GbaKeyMappings) -> Result<Key> {
    match key_str.to_uppercase().as_str() {
        // GBA Controller mappings using the button names/numbers you specified
        "A" | "0" => parse_raw_key(&key_mappings.a), // A button
        "B" | "1" => parse_raw_key(&key_mappings.b), // B button
        "E" | "2" => parse_raw_key(&key_mappings.select), // Select button
        "S" | "3" => parse_raw_key(&key_mappings.start), // Start button
        "R" | "4" => parse_raw_key(&key_mappings.right), // D-pad Right
        "L" | "5" => parse_raw_key(&key_mappings.left), // D-pad Left
        "U" | "6" => parse_raw_key(&key_mappings.up), // D-pad Up
        "D" | "7" => parse_raw_key(&key_mappings.down), // D-pad Down
        "I" | "8" => parse_raw_key(&key_mappings.r_shoulder), // Right shoulder
        "J" | "9" => parse_raw_key(&key_mappings.l_shoulder), // Left shoulder

        // Fall back to raw key parsing for regular keyboard keys
        _ => parse_raw_key(key_str),
    }
}

/// Executes a sequence of input actions using enigo
async fn execute_input_sequence(actions: &[InputAction]) -> Result<()> {
    if actions.is_empty() {
        return Ok(());
    }

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| anyhow::anyhow!("Failed to initialize input system: {}", e))?;

    for action in actions {
        match action {
            InputAction::Press { key, duration_ms } => {
                match duration_ms {
                    Some(duration) => {
                        // Hold key for specified duration
                        enigo
                            .key(*key, Direction::Press)
                            .map_err(|e| anyhow::anyhow!("Failed to press key: {}", e))?;
                        sleep(Duration::from_millis(*duration)).await;
                        enigo
                            .key(*key, Direction::Release)
                            .map_err(|e| anyhow::anyhow!("Failed to release key: {}", e))?;
                    }
                    None => {
                        // Quick press and release
                        enigo
                            .key(*key, Direction::Click)
                            .map_err(|e| anyhow::anyhow!("Failed to click key: {}", e))?;
                    }
                }
            }
            InputAction::KeyDown { key } => {
                enigo
                    .key(*key, Direction::Press)
                    .map_err(|e| anyhow::anyhow!("Failed to press key down: {}", e))?;
            }
            InputAction::KeyUp { key } => {
                enigo
                    .key(*key, Direction::Release)
                    .map_err(|e| anyhow::anyhow!("Failed to release key: {}", e))?;
            }
            InputAction::Wait { duration_ms } => {
                sleep(Duration::from_millis(*duration_ms)).await;
            }
        }
    }

    Ok(())
}

/// Main entry point: validates directory, discovers binaries, and captures GIFs
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Set up signal handling for graceful shutdown
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    tokio::spawn(async move {
        signal::ctrl_c().await.expect("Failed to listen for ctrl+c");
        println!("\nReceived Ctrl+C, shutting down gracefully...");
        shutdown_clone.store(true, Ordering::Relaxed);
    });

    // Use current directory if no project directory is provided
    let project_dir = args
        .project_dir
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    if !project_dir.exists() {
        return Err(anyhow::anyhow!(
            "Directory does not exist: {}",
            project_dir.display()
        ));
    }

    if !is_agbrs_project_dir(&project_dir) {
        return Err(anyhow::anyhow!(
            "Directory does not appear to be an agbrs project: {}",
            project_dir.display()
        ));
    }

    let frame_count = (args.fps * args.duration).ceil() as u32;
    let frame_delay_ms = (1000.0 / args.fps) as u64;

    println!("Using agbrs project at: {}", project_dir.display());
    println!(
        "GIF settings: {}fps, {}s duration, {} frames",
        args.fps, args.duration, frame_count
    );

    std::fs::create_dir_all("out")?;

    let binaries = discover_binaries(&project_dir)?;
    if binaries.is_empty() {
        return Err(anyhow::anyhow!(
            "No binary files found in {}/src/bin/ or {}/src/main.rs",
            project_dir.display(),
            project_dir.display()
        ));
    }

    println!("Found {} binaries: {}", binaries.len(), binaries.join(", "));

    println!("Setting up GBA development environment...");
    setup_gba_target().await?;
    println!("Pre-building all GBA binaries...");
    prebuild_binaries(&binaries, &project_dir).await?;
    println!("All binaries built successfully!\n");

    // Load capture configuration from capture.json if it exists
    let capture_config = load_capture_config(&project_dir)?;
    if capture_config.is_some() {
        println!("Using capture.json configuration file");
    }

    for binary in &binaries {
        // Check for shutdown signal before starting each binary
        if shutdown.load(Ordering::Relaxed) {
            println!("Shutdown requested, stopping capture process.");
            break;
        }

        println!("Capturing {}...", binary);

        // Get input sequences and key mappings for this specific binary
        let (before_input, during_input) = get_binary_input_sequences(
            binary,
            &capture_config,
            &args.before_capture,
            &args.during_capture,
        );

        let key_mappings = get_effective_key_mappings(binary, &capture_config);

        // Parse input sequences with key mappings
        let before_capture_actions = if let Some(ref input) = before_input {
            parse_input_sequence(input, &key_mappings)?
        } else {
            Vec::new()
        };

        let during_capture_actions = if let Some(ref input) = during_input {
            parse_input_sequence(input, &key_mappings)?
        } else {
            Vec::new()
        };

        // Show what input sequences will be used for this binary
        if !before_capture_actions.is_empty() {
            println!(
                "  Before-capture sequence: {}",
                before_input.as_ref().unwrap()
            );
        }
        if !during_capture_actions.is_empty() {
            println!(
                "  During-capture sequence: {}",
                during_input.as_ref().unwrap()
            );
        }

        capture_binary_gif(
            binary,
            &project_dir,
            frame_count,
            frame_delay_ms,
            &before_capture_actions,
            &during_capture_actions,
            &shutdown,
        )
        .await?;
        println!();
    }

    println!("All GIFs created successfully in out/ directory!");
    Ok(())
}

/// Discovers all Rust binary files in src/bin directory or src/main.rs
fn discover_binaries(project_dir: &Path) -> Result<Vec<String>> {
    let src_bin_dir = project_dir.join("src/bin");
    let src_main = project_dir.join("src/main.rs");
    let mut binaries = Vec::new();

    // Check for src/bin/*.rs files first
    if src_bin_dir.exists() {
        for entry in std::fs::read_dir(&src_bin_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(extension) = path.extension() {
                    if extension == "rs" {
                        if let Some(file_name) = path.file_stem() {
                            if let Some(binary_name) = file_name.to_str() {
                                binaries.push(binary_name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // If no binaries found in src/bin/, check for src/main.rs
    if binaries.is_empty() && src_main.exists() {
        // For src/main.rs projects, use the package name from Cargo.toml
        let cargo_toml_path = project_dir.join("Cargo.toml");
        if let Ok(cargo_content) = std::fs::read_to_string(&cargo_toml_path) {
            // Parse the package name from Cargo.toml
            for line in cargo_content.lines() {
                if line.trim().starts_with("name") {
                    if let Some(name_part) = line.split('=').nth(1) {
                        let name = name_part.trim().trim_matches('"').trim_matches('\'');
                        binaries.push(name.to_string());
                        break;
                    }
                }
            }
        }

        // Fallback to directory name if package name not found
        if binaries.is_empty() {
            if let Some(dir_name) = project_dir.file_name() {
                if let Some(name_str) = dir_name.to_str() {
                    binaries.push(name_str.to_string());
                }
            }
        }
    }

    binaries.sort();
    Ok(binaries)
}

/// Validates that a directory contains an agbrs project
fn is_agbrs_project_dir(path: &Path) -> bool {
    let cargo_toml = path.join("Cargo.toml");
    let src_bin = path.join("src/bin");
    let src_main = path.join("src/main.rs");
    let cargo_config = path.join(".cargo/config.toml");

    // Must have Cargo.toml and either src/bin/ or src/main.rs
    if !cargo_toml.exists() || (!src_bin.exists() && !src_main.exists()) {
        return false;
    }

    // Look for GBA-specific configuration
    if let Ok(config_content) = std::fs::read_to_string(&cargo_config) {
        if config_content.contains("thumbv4t-none-eabi") || config_content.contains("mgba") {
            return true;
        }
    }

    false
}

/// Ensures nightly toolchain is installed (required for GBA build-std)
async fn setup_gba_target() -> Result<()> {
    println!("Checking nightly toolchain for GBA development...");

    let output = Command::new("rustup")
        .args(&["toolchain", "list"])
        .output()?;

    let toolchains = String::from_utf8_lossy(&output.stdout);

    if !toolchains.contains("nightly") {
        println!("Installing nightly toolchain (required for build-std)...");
        let output = Command::new("rustup")
            .args(&["toolchain", "install", "nightly"])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "Failed to install nightly toolchain: {}",
                stderr
            ));
        }
        println!("Nightly toolchain installed successfully!");
    } else {
        println!("Nightly toolchain is available.");
    }

    Ok(())
}

/// Pre-builds all binaries to eliminate compilation delays during capture
async fn prebuild_binaries(binaries: &[String], project_dir: &Path) -> Result<()> {
    let has_src_bin = project_dir.join("src/bin").exists();

    for binary in binaries {
        println!("Building {}...", binary);
        let mut args = vec!["+nightly", "build", "--release"];

        // Only use --bin flag for src/bin projects
        if has_src_bin {
            args.extend(["--bin", binary]);
        }

        let output = Command::new("cargo")
            .current_dir(project_dir)
            .args(&args)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to build {}: {}", binary, stderr));
        }
    }
    Ok(())
}

/// Captures frames from an mGBA window and creates a GIF with configurable settings
async fn capture_binary_gif(
    binary_name: &String,
    project_dir: &Path,
    frame_count: u32,
    frame_delay_ms: u64,
    before_capture_actions: &[InputAction],
    during_capture_actions: &[InputAction],
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let has_src_bin = project_dir.join("src/bin").exists();
    let mut args = vec!["+nightly", "run", "--release"];

    // Only use --bin flag for src/bin projects
    if has_src_bin {
        args.extend(["--bin", binary_name]);
    }

    let mut child = Command::new("cargo")
        .current_dir(project_dir)
        .args(&args)
        .spawn()?;

    println!("Waiting for mGBA to start...");
    sleep(Duration::from_secs(2)).await;

    // Check for shutdown during initial wait
    if shutdown.load(Ordering::Relaxed) {
        println!("Shutdown requested, terminating mGBA process...");
        let _ = child.kill();
        return Ok(());
    }

    // Retry finding mGBA window up to 10 times
    let mut attempts = 0;
    let max_attempts = 10;

    loop {
        // Check for shutdown during window search
        if shutdown.load(Ordering::Relaxed) {
            println!("Shutdown requested, terminating mGBA process...");
            let _ = child.kill();
            return Ok(());
        }

        attempts += 1;
        match find_mgba_window() {
            Ok(_) => {
                println!("mGBA window found!");
                break;
            }
            Err(_) if attempts < max_attempts => {
                println!(
                    "mGBA window not found yet, waiting... (attempt {}/{})",
                    attempts, max_attempts
                );
                sleep(Duration::from_secs(1)).await;
                continue;
            }
            Err(e) => {
                let _ = child.kill();
                return Err(anyhow::anyhow!(
                    "Failed to find mGBA window after {} attempts: {}",
                    max_attempts,
                    e
                ));
            }
        }
    }

    // Execute before-capture input sequence
    if !before_capture_actions.is_empty() {
        println!("Executing before-capture input sequence...");
        execute_input_sequence(before_capture_actions).await?;
        println!("Before-capture input sequence completed.");
    }

    let gif_path = format!("out/{}.gif", binary_name);
    let mut gif_file = File::create(&gif_path)?;

    // Capture first frame to determine GIF dimensions
    let first_frame = find_mgba_window()?.capture_image()?;
    let first_frame: RgbaImage = ImageBuffer::from_raw(
        first_frame.width(),
        first_frame.height(),
        first_frame.into_raw(),
    )
    .ok_or_else(|| anyhow::anyhow!("Failed to convert first frame to RgbaImage"))?;
    let width = first_frame.width() as u16;
    let height = first_frame.height() as u16;

    let mut encoder = Encoder::new(&mut gif_file, width, height, &[])?;
    encoder.set_repeat(Repeat::Infinite)?;

    println!("Creating GIF {}x{} for {}", width, height, binary_name);

    add_frame_to_gif(&mut encoder, first_frame, frame_delay_ms)?;

    // Capture remaining frames in parallel with time offsets
    let remaining_frames = frame_count - 1;
    println!(
        "Starting parallel capture of {} frames...",
        remaining_frames
    );

    // Start during-capture input sequence in parallel if provided
    let input_task = if !during_capture_actions.is_empty() {
        println!("Starting during-capture input sequence...");
        Some(tokio::spawn({
            let actions = during_capture_actions.to_vec();
            async move { execute_input_sequence(&actions).await }
        }))
    } else {
        None
    };

    let mut tasks = Vec::new();

    for i in 1..frame_count {
        let delay_ms = (i as u64) * frame_delay_ms;
        let task = tokio::spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            let image = find_mgba_window()?.capture_image()?;
            let rgba_image: RgbaImage =
                ImageBuffer::from_raw(image.width(), image.height(), image.into_raw())
                    .ok_or_else(|| anyhow::anyhow!("Failed to convert frame {} to RgbaImage", i))?;
            Ok::<(u32, RgbaImage), anyhow::Error>((i, rgba_image))
        });
        tasks.push(task);
    }

    println!("Waiting for all frames to be captured...");
    let mut frames = Vec::with_capacity(remaining_frames as usize);

    for task in tasks {
        let result = task.await??;
        frames.push(result);
    }

    // Handle during-capture input task completion
    if let Some(task) = input_task {
        match task.await {
            Ok(Ok(())) => println!("During-capture input sequence completed successfully."),
            Ok(Err(e)) => println!("During-capture input sequence failed: {}", e),
            Err(e) => println!("During-capture input task panicked: {}", e),
        }
    }

    // Close mGBA window immediately after capture is complete
    let _ = child.kill();
    println!("Frame capture complete! mGBA window closed.");

    // Ensure frames are in correct chronological order
    frames.sort_by_key(|(index, _)| *index);

    println!("Building GIF from {} captured frames...", frame_count);
    for (index, frame) in frames {
        add_frame_to_gif(&mut encoder, frame, frame_delay_ms)?;
        if index % 10 == 0 {
            println!(
                "Added frame {}/{} to GIF for {}",
                index + 1,
                frame_count,
                binary_name
            );
        }
    }

    println!("Created GIF: {}", gif_path);
    Ok(())
}

/// Converts RGBA image to GIF frame and adds to encoder with configurable timing
fn add_frame_to_gif(
    encoder: &mut Encoder<&mut File>,
    rgba_image: RgbaImage,
    frame_delay_ms: u64,
) -> Result<()> {
    // Convert RGBA to RGB (GIF doesn't support alpha channel)
    let rgb_image: RgbImage =
        ImageBuffer::from_fn(rgba_image.width(), rgba_image.height(), |x, y| {
            let rgba_pixel = rgba_image.get_pixel(x, y);
            image::Rgb([rgba_pixel[0], rgba_pixel[1], rgba_pixel[2]])
        });

    let mut frame = Frame::from_rgb(
        rgb_image.width() as u16,
        rgb_image.height() as u16,
        rgb_image.as_raw(),
    );
    frame.delay = (frame_delay_ms / 10) as u16; // Convert ms to centiseconds

    encoder.write_frame(&frame)?;
    Ok(())
}

/// Finds the first window with "mgba" in the title (case-insensitive)
fn find_mgba_window() -> Result<Window> {
    let windows = Window::all()?;

    for window in windows {
        let title = window.title();
        if title.to_lowercase().contains("mgba") {
            return Ok(window);
        }
    }

    Err(anyhow::anyhow!("mGBA window not found"))
}
