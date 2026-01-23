//! Client for receiving CMAF video stream over xoq (QUIC transport).
//!
//! This example demonstrates receiving H.264 video streamed as CMAF
//! segments over QUIC using either:
//! - **MoQ (relay)**: Subscribes to a broadcast and receives groups
//! - **iroh (P2P)**: Direct peer-to-peer with length-prefixed framing
//!
//! # CMAF Structure
//!
//! - **First segment**: Initialization segment (ftyp + moov)
//! - **Subsequent segments**: Media segments (moof + mdat)
//!
//! # Usage
//!
//! ```bash
//! # MoQ mode (relay) - default
//! cargo run --example camera_xoq_client --features xoq
//! cargo run --example camera_xoq_client --features xoq -- anon/camera
//! cargo run --example camera_xoq_client --features xoq -- --relay https://localhost:4443 anon/camera
//!
//! # iroh mode (P2P client)
//! cargo run --example camera_xoq_client --features xoq -- --iroh <SERVER_ID>
//!
//! # Save to file
//! cargo run --example camera_xoq_client --features xoq -- --output video.mp4 anon/camera
//! ```

use anyhow::Result;
use moq_native::moq_lite::{Origin, Track};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use xoq::{IrohClientBuilder, IrohStream};

// Statistics
static SEGMENTS_RECEIVED: AtomicUsize = AtomicUsize::new(0);
static BYTES_RECEIVED: AtomicUsize = AtomicUsize::new(0);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

/// Output writer for received segments
struct SegmentWriter {
    file: Option<File>,
    init_received: bool,
}

impl SegmentWriter {
    fn new(output_path: Option<PathBuf>) -> Result<Self> {
        let file = output_path
            .as_ref()
            .map(|p| File::create(p))
            .transpose()?;
        Ok(Self {
            file,
            init_received: false,
        })
    }

    fn write_segment(&mut self, data: &[u8], is_init: bool) -> Result<()> {
        let segment_num = SEGMENTS_RECEIVED.fetch_add(1, Ordering::SeqCst);
        BYTES_RECEIVED.fetch_add(data.len(), Ordering::SeqCst);

        if is_init {
            self.init_received = true;
            println!(
                "  Received init segment #{} ({} bytes)",
                segment_num,
                data.len()
            );
        } else {
            println!(
                "  Received media segment #{} ({} bytes)",
                segment_num,
                data.len()
            );
        }

        // Write to file if configured
        if let Some(ref mut file) = self.file {
            file.write_all(data)?;
            file.flush()?;
        }

        Ok(())
    }
}

/// Read a length-prefixed frame from iroh stream
async fn read_iroh_frame(stream: &mut IrohStream) -> Result<Option<Vec<u8>>> {
    // Read 4-byte length prefix
    let mut len_buf = [0u8; 4];
    let mut offset = 0;
    while offset < 4 {
        match stream.read(&mut len_buf[offset..]).await? {
            Some(n) if n > 0 => offset += n,
            _ => return Ok(None), // Connection closed
        }
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(Some(Vec::new()));
    }

    // Read frame data
    let mut data = vec![0u8; len];
    let mut offset = 0;
    while offset < len {
        match stream.read(&mut data[offset..]).await? {
            Some(n) if n > 0 => offset += n,
            _ => return Ok(None), // Connection closed
        }
    }

    Ok(Some(data))
}

/// Run the client in iroh P2P mode
async fn run_iroh_client(server_id: &str, writer: Arc<Mutex<SegmentWriter>>) -> Result<()> {
    println!("Connecting to iroh server: {}...", server_id);

    let conn = IrohClientBuilder::new().connect_str(server_id).await?;
    println!("Connected to server: {}", conn.remote_id());

    // Accept stream from server (server pushes video to us)
    println!("Waiting for server to open stream...");
    let mut stream = conn.accept_stream().await?;
    println!("Stream received. Receiving segments...\n");

    let mut is_first = true;
    while !SHOULD_STOP.load(Ordering::SeqCst) {
        match read_iroh_frame(&mut stream).await? {
            Some(data) if !data.is_empty() => {
                let mut w = writer.lock().await;
                w.write_segment(&data, is_first)?;
                is_first = false;
            }
            Some(_) => {
                // Empty frame, continue
            }
            None => {
                println!("\nConnection closed by server.");
                break;
            }
        }
    }

    Ok(())
}

/// Run the client in MoQ relay mode
async fn run_moq_client(
    relay_url: Option<&str>,
    path: &str,
    writer: Arc<Mutex<SegmentWriter>>,
) -> Result<()> {
    let relay = relay_url.unwrap_or("https://cdn.moq.dev");
    println!("Connecting to MoQ relay: {}", relay);
    println!("Subscribing to path: {}", path);

    let url_str = match relay_url {
        Some(url) => format!("{}/{}", url, path),
        None => format!("https://cdn.moq.dev/{}", path),
    };
    let url = url::Url::parse(&url_str)?;

    let client = moq_native::ClientConfig::default().init()?;
    let mut origin = Origin::produce();

    // Connect as subscriber (None for publish, origin.producer for subscribe)
    let _session = client.connect(url.clone(), None, origin.producer).await?;
    println!("Connected to: {}", url);
    println!("Waiting for broadcast announcement...\n");

    // Wait for a broadcast to be announced (with timeout/retry)
    let broadcast = loop {
        if SHOULD_STOP.load(Ordering::SeqCst) {
            println!("Stopped while waiting for broadcast.");
            return Ok(());
        }

        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            origin.consumer.announced(),
        )
        .await
        {
            Ok(Some((announced_path, Some(broadcast))) ) => {
                println!("Broadcast announced at path: {:?}", announced_path);
                break broadcast;
            }
            Ok(Some((announced_path, None))) => {
                println!("Broadcast at {:?} ended, waiting for new one...", announced_path);
                continue;
            }
            Ok(None) => {
                println!("Origin closed. Is the server running?");
                return Ok(());
            }
            Err(_) => {
                println!("  Waiting for broadcast... (server may not be streaming yet)");
                continue;
            }
        }
    };

    // Subscribe to the video track
    let track_info = Track {
        name: "video".to_string(),
        priority: 0,
    };
    let mut track = broadcast.subscribe_track(&track_info);
    println!("Subscribed to video track. Receiving segments...\n");

    let mut is_first = true;
    let mut group_count = 0u64;

    println!("Waiting for video segments...");
    println!("(Make sure the server is actively streaming)\n");

    while !SHOULD_STOP.load(Ordering::SeqCst) {
        // Get next group with timeout for better feedback
        let group_result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            track.next_group(),
        )
        .await;

        match group_result {
            Ok(Ok(Some(mut group))) => {
                group_count += 1;

                // Read all frames from this group
                loop {
                    match group.read_frame().await {
                        Ok(Some(data)) => {
                            let mut w = writer.lock().await;
                            w.write_segment(&data, is_first)?;
                            is_first = false;
                        }
                        Ok(None) => {
                            // No more frames in this group
                            break;
                        }
                        Err(e) => {
                            eprintln!("  Error reading frame from group {}: {:?}", group_count, e);
                            break;
                        }
                    }
                }
            }
            Ok(Ok(None)) => {
                if group_count == 0 {
                    println!("\nTrack ended without sending any data.");
                    println!("This usually means:");
                    println!("  - The server finished streaming before you connected");
                    println!("  - The server isn't actively streaming");
                    println!("\nTry: Start the client WHILE the server is streaming.");
                } else {
                    println!("\nTrack ended after {} groups.", group_count);
                }
                break;
            }
            Ok(Err(e)) => {
                eprintln!("\nError getting next group: {:?}", e);
                if group_count == 0 {
                    eprintln!("No data received. Server may have disconnected.");
                }
                break;
            }
            Err(_) => {
                // Timeout - no data in 10 seconds
                if group_count == 0 {
                    println!("  Still waiting for first segment... (10s timeout)");
                    println!("  Hint: Is the server actively streaming?");
                } else {
                    println!("  No new segments in 10s (received {} so far)", group_count);
                }
                // Continue waiting
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!("Usage: camera_xoq_client [OPTIONS] [PATH_OR_SERVER_ID]");
    println!();
    println!("Arguments:");
    println!("  [PATH_OR_SERVER_ID]  MoQ path (default: anon/camera) or iroh server ID");
    println!();
    println!("Options:");
    println!("  --relay <URL>        Custom relay URL (default: https://cdn.moq.dev)");
    println!("  --iroh               Use iroh P2P mode (requires server ID argument)");
    println!("  --output <FILE>      Save received video to file (CMAF/fragmented MP4)");
    println!("  -h, --help           Show this help message");
    println!();
    println!("Transport Modes:");
    println!("  MoQ (default):       Relay-based pub/sub - subscribes to broadcast");
    println!("  iroh (--iroh):       Direct P2P - connects to server by ID");
    println!();
    println!("Examples:");
    println!("  camera_xoq_client                              # Subscribe via MoQ relay");
    println!("  camera_xoq_client anon/my-camera               # Subscribe to custom path");
    println!("  camera_xoq_client --relay https://... path     # Use custom relay");
    println!("  camera_xoq_client --iroh <SERVER_ID>           # Connect P2P to server");
    println!("  camera_xoq_client --output video.mp4           # Save to file");
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "warn");
    }
    tracing_subscriber::fmt::init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    let mut path_or_id = "anon/camera";
    let mut relay_url: Option<&str> = None;
    let mut use_iroh = false;
    let mut output_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--relay" => {
                if i + 1 < args.len() {
                    relay_url = Some(&args[i + 1]);
                    i += 2;
                } else {
                    eprintln!("Error: --relay requires a URL argument");
                    std::process::exit(1);
                }
            }
            "--iroh" => {
                use_iroh = true;
                i += 1;
            }
            "--output" | "-o" => {
                if i + 1 < args.len() {
                    output_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    eprintln!("Error: --output requires a file path argument");
                    std::process::exit(1);
                }
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            _ => {
                path_or_id = &args[i];
                i += 1;
            }
        }
    }

    // Validate iroh mode has server ID
    if use_iroh && path_or_id == "anon/camera" {
        eprintln!("Error: --iroh mode requires a server ID argument");
        eprintln!("Run 'camera_xoq_stream --iroh' first and use the displayed Server ID");
        std::process::exit(1);
    }

    println!("Camera CMAF Client over xoq");
    println!("===========================");
    println!(
        "Transport: {}",
        if use_iroh { "iroh (P2P)" } else { "MoQ (relay)" }
    );
    if let Some(ref path) = output_path {
        println!("Output: {}", path.display());
    } else {
        println!("Output: (not saving to file)");
    }
    println!();

    // Set up Ctrl+C handler
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C, stopping...");
        stop_flag_clone.store(true, Ordering::SeqCst);
        SHOULD_STOP.store(true, Ordering::SeqCst);
    })?;

    // Create segment writer
    let writer = Arc::new(Mutex::new(SegmentWriter::new(output_path.clone())?));

    // Run appropriate client mode
    let result = if use_iroh {
        run_iroh_client(path_or_id, writer.clone()).await
    } else {
        run_moq_client(relay_url, path_or_id, writer.clone()).await
    };

    // Print statistics
    let segments = SEGMENTS_RECEIVED.load(Ordering::SeqCst);
    let bytes = BYTES_RECEIVED.load(Ordering::SeqCst);

    println!();
    println!("===========================");
    println!("Session complete!");
    println!("  Segments received: {}", segments);
    println!(
        "  Total bytes: {} ({:.2} MB)",
        bytes,
        bytes as f64 / 1_048_576.0
    );
    if let Some(ref path) = output_path {
        println!("  Saved to: {}", path.display());
        println!();
        println!("To play the video:");
        println!("  ffplay {}", path.display());
        println!("  # or");
        println!("  vlc {}", path.display());
    }

    result
}
