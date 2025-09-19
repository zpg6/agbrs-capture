//! GBA GIF Capture Tool
//!
//! Captures frames from mGBA windows and creates GIFs automatically
//! for each binary in an agbrs project.

use anyhow::Result;
use clap::Parser;
use gif::{Encoder, Frame, Repeat};
use image::{ImageBuffer, RgbImage, RgbaImage};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::time::sleep;
use xcap::Window;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(about = "Captures frames from mGBA windows and creates GIFs for agbrs binaries")]
struct Args {
    /// Path to the agbrs project directory
    #[arg(help = "Directory containing Cargo.toml and src/bin/ with agbrs binaries")]
    project_dir: PathBuf,

    /// Frames per second for the output GIF
    #[arg(long, default_value_t = 10.0)]
    #[arg(help = "GIF framerate (frames per second)")]
    fps: f32,

    /// Duration of the GIF in seconds
    #[arg(long, default_value_t = 3.0)]
    #[arg(help = "GIF duration in seconds")]
    duration: f32,
}

/// Main entry point: validates directory, discovers binaries, and captures GIFs
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if !args.project_dir.exists() {
        return Err(anyhow::anyhow!(
            "Directory does not exist: {}",
            args.project_dir.display()
        ));
    }

    if !is_agbrs_project_dir(&args.project_dir) {
        return Err(anyhow::anyhow!(
            "Directory does not appear to be an agbrs project: {}",
            args.project_dir.display()
        ));
    }

    let frame_count = (args.fps * args.duration).ceil() as u32;
    let frame_delay_ms = (1000.0 / args.fps) as u64;

    println!("Using agbrs project at: {}", args.project_dir.display());
    println!(
        "GIF settings: {}fps, {}s duration, {} frames",
        args.fps, args.duration, frame_count
    );

    std::fs::create_dir_all("out")?;

    let binaries = discover_binaries(&args.project_dir)?;
    if binaries.is_empty() {
        return Err(anyhow::anyhow!(
            "No binary files found in {}/src/bin/",
            args.project_dir.display()
        ));
    }

    println!("Found {} binaries: {}", binaries.len(), binaries.join(", "));

    println!("Setting up GBA development environment...");
    setup_gba_target().await?;
    println!("Pre-building all GBA binaries...");
    prebuild_binaries(&binaries, &args.project_dir).await?;
    println!("All binaries built successfully!\n");

    for binary in &binaries {
        println!("Capturing {}...", binary);
        capture_binary_gif(binary, &args.project_dir, frame_count, frame_delay_ms).await?;
        println!();
    }

    println!("All GIFs created successfully in out/ directory!");
    Ok(())
}

/// Discovers all Rust binary files in src/bin directory
fn discover_binaries(project_dir: &Path) -> Result<Vec<String>> {
    let src_bin_dir = project_dir.join("src/bin");

    if !src_bin_dir.exists() {
        return Ok(Vec::new());
    }

    let mut binaries = Vec::new();

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

    binaries.sort();
    Ok(binaries)
}

/// Validates that a directory contains an agbrs project
fn is_agbrs_project_dir(path: &Path) -> bool {
    let cargo_toml = path.join("Cargo.toml");
    let src_bin = path.join("src/bin");
    let cargo_config = path.join(".cargo/config.toml");

    if !cargo_toml.exists() || !src_bin.exists() {
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
    for binary in binaries {
        println!("Building {}...", binary);
        let output = Command::new("cargo")
            .current_dir(project_dir)
            .args(&["+nightly", "build", "--bin", binary, "--release"])
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
) -> Result<()> {
    let mut child = Command::new("cargo")
        .current_dir(project_dir)
        .args(&["+nightly", "run", "--bin", binary_name, "--release"])
        .spawn()?;

    println!("Waiting for mGBA to start...");
    sleep(Duration::from_secs(2)).await;

    // Retry finding mGBA window up to 10 times
    let mut attempts = 0;
    let max_attempts = 10;

    loop {
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
                child.kill()?;
                return Err(anyhow::anyhow!(
                    "Failed to find mGBA window after {} attempts: {}",
                    max_attempts,
                    e
                ));
            }
        }
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

    // Ensure frames are in correct chronological order
    frames.sort_by_key(|(index, _)| *index);

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

    child.kill()?;

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
