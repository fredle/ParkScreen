mod capture;
mod display;
mod encode;
mod pipeline;
mod transport;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use display::{Mode, Selector};
use std::net::{IpAddr, SocketAddr, UdpSocket};

#[derive(Parser)]
#[command(name = "parkscreen-host", version, about = "Stream a Windows monitor to a browser (the Tesla) over WebRTC")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List monitors attached to the desktop.
    List,
    /// Change a monitor's resolution, e.g. `set-mode --monitor 2 1920x1200@60`.
    SetMode {
        /// Monitor index (from `list`), GDI name (\\.\DISPLAY3) or part of its name.
        #[arg(long)]
        monitor: Selector,
        /// Mode such as 1920x1200 or 1920x1200@60.
        mode: Mode,
    },
    /// Capture a monitor and serve it to browsers on the local network.
    Serve {
        /// Monitor index, GDI name, part of its name, or `auto` (a virtual display if present).
        #[arg(long, default_value = "auto")]
        monitor: Selector,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Target bitrate, e.g. 12M or 8000k.
        #[arg(long, default_value = "12M", value_parser = parse_bitrate)]
        bitrate: u32,
        /// Switch the monitor to the viewer's reported screen size when it connects.
        #[arg(long)]
        match_viewport: bool,
        /// HTTP port for the player page and signalling.
        #[arg(long, default_value_t = 8765)]
        port: u16,
        /// Fixed UDP port for WebRTC media (open it in the firewall).
        #[arg(long, default_value_t = 8766)]
        udp_port: u16,
    },
}

/// Parse `12M`, `8000k`, `5000000` into bits per second.
fn parse_bitrate(s: &str) -> Result<u32, String> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mult) = match s.strip_suffix('m') {
        Some(n) => (n, 1_000_000.0),
        None => match s.strip_suffix('k') {
            Some(n) => (n, 1_000.0),
            None => (s.as_str(), 1.0),
        },
    };
    let v: f64 = num.parse().map_err(|_| format!("bad bitrate '{s}' (try 12M or 8000k)"))?;
    if v <= 0.0 {
        return Err("bitrate must be positive".into());
    }
    Ok((v * mult) as u32)
}

/// The address other devices on the LAN would use to reach this PC.
fn lan_ip() -> Option<IpAddr> {
    // Connecting a UDP socket sends nothing; it just makes the OS pick the outbound interface.
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    Some(sock.local_addr().ok()?.ip())
}

#[cfg(windows)]
fn init_dpi() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwareness, PROCESS_PER_MONITOR_DPI_AWARE};
    // Report real pixels, not scaled ones. Fails harmlessly if already set.
    let _ = unsafe { SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE) };
}

#[cfg(not(windows))]
fn init_dpi() {}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "parkscreen_host=info,webrtc::peer_connection::driver=off,rtc_mdns=off,warn".into()),
        )
        .init();
    init_dpi();

    match Cli::parse().command {
        Cmd::List => {
            let monitors = display::enumerate()?;
            if monitors.is_empty() {
                bail!("no monitors found");
            }
            for m in &monitors {
                println!(
                    "{}: {}  \"{}\"  [{}]  {}x{}@{}Hz at ({},{}){}{}",
                    m.index,
                    m.device_name,
                    m.friendly_name,
                    m.adapter,
                    m.width,
                    m.height,
                    m.hz,
                    m.x,
                    m.y,
                    if m.primary { "  primary" } else { "" },
                    if m.is_virtual() { "  virtual?" } else { "" },
                );
            }
        }
        Cmd::SetMode { monitor, mode } => {
            let monitors = display::enumerate()?;
            let m = display::select(&monitors, &monitor)?;
            display::set_mode(&m.device_name, mode)?;
            println!("{} set to {mode}", m.device_name);
        }
        Cmd::Serve { monitor, fps, bitrate, match_viewport, port, udp_port } => {
            let monitors = display::enumerate()?;
            let m = display::select(&monitors, &monitor)?.clone();
            println!(
                "Capturing monitor {} {} \"{}\" ({}x{})",
                m.index, m.device_name, m.friendly_name, m.width, m.height
            );
            let pipeline = pipeline::Pipeline::start(
                m.device_name.clone(),
                encode::EncoderSettings { fps, bitrate_bps: bitrate },
            );
            let ip = lan_ip();
            match ip {
                Some(ip) => println!("\nOpen in the car (parked!) or any browser on this network:\n\n    http://{ip}:{port}\n"),
                None => println!("\nListening on port {port}.\n"),
            }
            println!("If the connection stalls, allow UDP {udp_port} and TCP {port} through Windows Firewall.");
            transport::serve(
                pipeline,
                transport::ServerConfig {
                    bind: SocketAddr::from(([0, 0, 0, 0], port)),
                    udp_port,
                    match_viewport,
                },
            )
            .await
            .context("server stopped")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bitrates() {
        assert_eq!(parse_bitrate("12M").unwrap(), 12_000_000);
        assert_eq!(parse_bitrate("8000k").unwrap(), 8_000_000);
        assert_eq!(parse_bitrate("2.5m").unwrap(), 2_500_000);
        assert_eq!(parse_bitrate("5000000").unwrap(), 5_000_000);
        assert!(parse_bitrate("fast").is_err());
        assert!(parse_bitrate("0").is_err());
    }
}
