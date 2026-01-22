//! CMAF (Common Media Application Format) muxer for H.264 video streams.
//!
//! This module provides a pure-Rust CMAF muxer suitable for:
//! - Live streaming (DASH/HLS)
//! - Media Source Extensions (MSE) in browsers
//! - Low-latency video delivery
//!
//! # CMAF Structure
//!
//! ```text
//! Initialization Segment:
//!   ftyp (file type)
//!   moov (movie header with track info, SPS/PPS)
//!
//! Media Segments:
//!   styp (segment type, optional)
//!   moof (movie fragment header)
//!   mdat (media data - encoded NAL units)
//! ```
//!
//! # Example
//!
//! ```no_run
//! use video_toolbox_sys::helpers::cmaf_muxer::{CmafMuxer, CmafConfig};
//!
//! let mut muxer = CmafMuxer::new(CmafConfig {
//!     fragment_duration_ms: 2000,
//!     timescale: 90000,
//! });
//!
//! // Create initialization segment with SPS/PPS
//! // let init_segment = muxer.create_init_segment(&sps, &pps, 1920, 1080);
//!
//! // Add frames, get media segments when ready
//! // if let Some(segment) = muxer.add_frame(&nal_units, pts, dts, is_keyframe) {
//! //     // Write segment to file or send over network
//! // }
//! ```

use super::nal_extractor::NalUnit;

/// Configuration for the CMAF muxer.
#[derive(Debug, Clone)]
pub struct CmafConfig {
    /// Target fragment duration in milliseconds.
    /// Fragments are aligned to keyframes, so actual duration may vary.
    pub fragment_duration_ms: u32,
    /// Timescale for timestamps (e.g., 90000 for standard video).
    pub timescale: u32,
}

impl Default for CmafConfig {
    fn default() -> Self {
        Self {
            fragment_duration_ms: 2000,
            timescale: 90000,
        }
    }
}

/// A pending frame waiting to be muxed.
#[derive(Debug, Clone)]
struct PendingFrame {
    /// Encoded NAL unit data (in AVCC format for mdat)
    data: Vec<u8>,
    /// Duration in timescale units
    duration: u32,
    /// Is this a sync sample (keyframe)
    is_sync: bool,
    /// Composition time offset (PTS - DTS)
    composition_offset: i32,
}

/// Fragmented MP4 muxer for H.264 video streams.
pub struct CmafMuxer {
    config: CmafConfig,
    /// Whether initialization segment has been created
    initialized: bool,
    /// Width in pixels
    width: u32,
    /// Height in pixels
    height: u32,
    /// SPS data (without NAL start code)
    sps: Vec<u8>,
    /// PPS data (without NAL start code)
    pps: Vec<u8>,
    /// Pending frames for current fragment
    pending_frames: Vec<PendingFrame>,
    /// Current fragment sequence number
    sequence_number: u32,
    /// Base DTS for current fragment
    fragment_base_dts: i64,
    /// Last frame's DTS
    last_dts: i64,
    /// Track ID
    track_id: u32,
}

impl CmafMuxer {
    /// Create a new CMAF muxer with the given configuration.
    pub fn new(config: CmafConfig) -> Self {
        Self {
            config,
            initialized: false,
            width: 0,
            height: 0,
            sps: Vec::new(),
            pps: Vec::new(),
            pending_frames: Vec::new(),
            sequence_number: 1,
            fragment_base_dts: 0,
            last_dts: 0,
            track_id: 1,
        }
    }

    /// Create the initialization segment (ftyp + moov).
    ///
    /// This must be called once before adding frames. The initialization segment
    /// contains codec configuration (SPS/PPS) and must be sent before any media
    /// segments.
    ///
    /// # Arguments
    /// * `sps` - H.264 Sequence Parameter Set (without NAL start code or length prefix)
    /// * `pps` - H.264 Picture Parameter Set (without NAL start code or length prefix)
    /// * `width` - Video width in pixels
    /// * `height` - Video height in pixels
    pub fn create_init_segment(&mut self, sps: &[u8], pps: &[u8], width: u32, height: u32) -> Vec<u8> {
        self.sps = sps.to_vec();
        self.pps = pps.to_vec();
        self.width = width;
        self.height = height;
        self.initialized = true;

        let mut buf = Vec::new();

        // ftyp box
        self.write_ftyp(&mut buf);

        // moov box
        self.write_moov(&mut buf);

        buf
    }

    /// Add an encoded frame to the muxer.
    ///
    /// Returns a media segment when enough frames have accumulated or when a
    /// new keyframe arrives after the target fragment duration.
    ///
    /// # Arguments
    /// * `nal_units` - NAL units for this frame (video slices, not SPS/PPS)
    /// * `pts` - Presentation timestamp in timescale units
    /// * `dts` - Decode timestamp in timescale units
    /// * `duration` - Frame duration in timescale units
    /// * `is_keyframe` - Whether this is a sync sample (IDR frame)
    pub fn add_frame(
        &mut self,
        nal_units: &[NalUnit],
        pts: i64,
        dts: i64,
        duration: u32,
        is_keyframe: bool,
    ) -> Option<Vec<u8>> {
        if !self.initialized {
            return None;
        }

        // Check if we should start a new fragment
        let should_flush = if self.pending_frames.is_empty() {
            false
        } else {
            // Flush if we have a keyframe and exceeded target duration
            let fragment_duration =
                (dts - self.fragment_base_dts) * 1000 / self.config.timescale as i64;
            is_keyframe && fragment_duration >= self.config.fragment_duration_ms as i64
        };

        let segment = if should_flush {
            Some(self.flush_fragment())
        } else {
            None
        };

        // Convert NAL units to AVCC format for mdat
        let data = self.nal_units_to_avcc(nal_units);

        // If this is the first frame in a fragment, record base DTS
        if self.pending_frames.is_empty() {
            self.fragment_base_dts = dts;
        }

        let composition_offset = (pts - dts) as i32;

        self.pending_frames.push(PendingFrame {
            data,
            duration,
            is_sync: is_keyframe,
            composition_offset,
        });

        self.last_dts = dts;

        segment
    }

    /// Flush any remaining frames as a final segment.
    ///
    /// Call this when encoding is complete to get the last fragment.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.pending_frames.is_empty() {
            return None;
        }
        Some(self.flush_fragment())
    }

    /// Convert NAL units to AVCC format (length-prefixed).
    fn nal_units_to_avcc(&self, nal_units: &[NalUnit]) -> Vec<u8> {
        let total_size: usize = nal_units
            .iter()
            .filter(|n| n.is_slice()) // Only include video slices
            .map(|n| 4 + n.data.len())
            .sum();

        let mut buf = Vec::with_capacity(total_size);

        for nal in nal_units.iter().filter(|n| n.is_slice()) {
            let len = nal.data.len() as u32;
            buf.extend_from_slice(&len.to_be_bytes());
            buf.extend_from_slice(&nal.data);
        }

        buf
    }

    /// Create a media segment from pending frames.
    fn flush_fragment(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Optional: styp box (some players require it)
        self.write_styp(&mut buf);

        // moof box
        self.write_moof(&mut buf);

        // mdat box
        self.write_mdat(&mut buf);

        self.sequence_number += 1;
        self.pending_frames.clear();

        buf
    }

    // ========================================
    // Box writing helpers
    // ========================================

    fn write_ftyp(&self, buf: &mut Vec<u8>) {
        let brands = [
            b"isom", // ISO Base Media
            b"iso6", // ISO with fragments
            b"cmfc", // CMAF compliant
            b"cmfv", // CMAF video track
            b"avc1", // H.264
            b"mp41", // MP4 v1
        ];

        let size = 8 + 4 + 4 + (brands.len() * 4);
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"ftyp");
        buf.extend_from_slice(b"isom"); // major brand
        buf.extend_from_slice(&0u32.to_be_bytes()); // minor version
        for brand in &brands {
            buf.extend_from_slice(*brand);
        }
    }

    fn write_styp(&self, buf: &mut Vec<u8>) {
        let brands = [
            b"msdh", // Media Segment Data Handler
            b"msix", // Media Segment Index
            b"cmfc", // CMAF compliant
            b"cmfv", // CMAF video track
        ];
        let size = 8 + 4 + 4 + (brands.len() * 4);
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"styp");
        buf.extend_from_slice(b"cmfv"); // major brand (CMAF video)
        buf.extend_from_slice(&0u32.to_be_bytes()); // minor version
        for brand in &brands {
            buf.extend_from_slice(*brand);
        }
    }

    fn write_moov(&self, buf: &mut Vec<u8>) {
        let mut moov_content = Vec::new();

        // mvhd (movie header)
        self.write_mvhd(&mut moov_content);

        // trak (track)
        self.write_trak(&mut moov_content);

        // mvex (movie extends - required for fragmented MP4)
        self.write_mvex(&mut moov_content);

        let size = 8 + moov_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"moov");
        buf.extend_from_slice(&moov_content);
    }

    fn write_mvhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags

        content.extend_from_slice(&0u32.to_be_bytes()); // creation time
        content.extend_from_slice(&0u32.to_be_bytes()); // modification time
        content.extend_from_slice(&self.config.timescale.to_be_bytes()); // timescale
        content.extend_from_slice(&0u32.to_be_bytes()); // duration (unknown for live)

        content.extend_from_slice(&0x00010000u32.to_be_bytes()); // rate (1.0)
        content.extend_from_slice(&0x0100u16.to_be_bytes()); // volume (1.0)
        content.extend_from_slice(&[0; 2]); // reserved
        content.extend_from_slice(&[0; 8]); // reserved

        // Matrix (identity)
        let matrix: [u32; 9] = [
            0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000,
        ];
        for m in &matrix {
            content.extend_from_slice(&m.to_be_bytes());
        }

        content.extend_from_slice(&[0; 24]); // pre_defined
        content.extend_from_slice(&2u32.to_be_bytes()); // next_track_id

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvhd");
        buf.extend_from_slice(&content);
    }

    fn write_trak(&self, buf: &mut Vec<u8>) {
        let mut trak_content = Vec::new();

        self.write_tkhd(&mut trak_content);
        self.write_mdia(&mut trak_content);

        let size = 8 + trak_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trak");
        buf.extend_from_slice(&trak_content);
    }

    fn write_tkhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 3]); // flags (track enabled, in movie)

        content.extend_from_slice(&0u32.to_be_bytes()); // creation time
        content.extend_from_slice(&0u32.to_be_bytes()); // modification time
        content.extend_from_slice(&self.track_id.to_be_bytes()); // track id
        content.extend_from_slice(&0u32.to_be_bytes()); // reserved
        content.extend_from_slice(&0u32.to_be_bytes()); // duration (unknown)

        content.extend_from_slice(&[0; 8]); // reserved
        content.extend_from_slice(&0i16.to_be_bytes()); // layer
        content.extend_from_slice(&0i16.to_be_bytes()); // alternate_group
        content.extend_from_slice(&0i16.to_be_bytes()); // volume (video = 0)
        content.extend_from_slice(&0u16.to_be_bytes()); // reserved

        // Matrix
        let matrix: [u32; 9] = [
            0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000,
        ];
        for m in &matrix {
            content.extend_from_slice(&m.to_be_bytes());
        }

        // Width and height as 16.16 fixed point
        content.extend_from_slice(&((self.width as u32) << 16).to_be_bytes());
        content.extend_from_slice(&((self.height as u32) << 16).to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tkhd");
        buf.extend_from_slice(&content);
    }

    fn write_mdia(&self, buf: &mut Vec<u8>) {
        let mut mdia_content = Vec::new();

        self.write_mdhd(&mut mdia_content);
        self.write_hdlr(&mut mdia_content);
        self.write_minf(&mut mdia_content);

        let size = 8 + mdia_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdia");
        buf.extend_from_slice(&mdia_content);
    }

    fn write_mdhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags

        content.extend_from_slice(&0u32.to_be_bytes()); // creation time
        content.extend_from_slice(&0u32.to_be_bytes()); // modification time
        content.extend_from_slice(&self.config.timescale.to_be_bytes()); // timescale
        content.extend_from_slice(&0u32.to_be_bytes()); // duration

        content.extend_from_slice(&0x55c4u16.to_be_bytes()); // language (und)
        content.extend_from_slice(&0u16.to_be_bytes()); // pre_defined

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdhd");
        buf.extend_from_slice(&content);
    }

    fn write_hdlr(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
        content.extend_from_slice(b"vide"); // handler_type
        content.extend_from_slice(&[0; 12]); // reserved
        content.extend_from_slice(b"VideoHandler\0"); // name

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"hdlr");
        buf.extend_from_slice(&content);
    }

    fn write_minf(&self, buf: &mut Vec<u8>) {
        let mut minf_content = Vec::new();

        self.write_vmhd(&mut minf_content);
        self.write_dinf(&mut minf_content);
        self.write_stbl(&mut minf_content);

        let size = 8 + minf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"minf");
        buf.extend_from_slice(&minf_content);
    }

    fn write_vmhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 1]); // flags
        content.extend_from_slice(&0u16.to_be_bytes()); // graphics_mode
        content.extend_from_slice(&[0; 6]); // opcolor

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"vmhd");
        buf.extend_from_slice(&content);
    }

    fn write_dinf(&self, buf: &mut Vec<u8>) {
        let mut dinf_content = Vec::new();

        // dref box
        let mut dref_content = Vec::new();
        dref_content.push(0); // version
        dref_content.extend_from_slice(&[0, 0, 0]); // flags
        dref_content.extend_from_slice(&1u32.to_be_bytes()); // entry_count

        // url entry (self-contained)
        dref_content.extend_from_slice(&12u32.to_be_bytes()); // size
        dref_content.extend_from_slice(b"url ");
        dref_content.push(0); // version
        dref_content.extend_from_slice(&[0, 0, 1]); // flags (self-contained)

        let dref_size = 8 + dref_content.len();
        dinf_content.extend_from_slice(&(dref_size as u32).to_be_bytes());
        dinf_content.extend_from_slice(b"dref");
        dinf_content.extend_from_slice(&dref_content);

        let size = 8 + dinf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"dinf");
        buf.extend_from_slice(&dinf_content);
    }

    fn write_stbl(&self, buf: &mut Vec<u8>) {
        let mut stbl_content = Vec::new();

        self.write_stsd(&mut stbl_content);
        self.write_empty_stts(&mut stbl_content);
        self.write_empty_stsc(&mut stbl_content);
        self.write_empty_stsz(&mut stbl_content);
        self.write_empty_stco(&mut stbl_content);

        let size = 8 + stbl_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stbl");
        buf.extend_from_slice(&stbl_content);
    }

    fn write_stsd(&self, buf: &mut Vec<u8>) {
        let mut stsd_content = Vec::new();

        stsd_content.push(0); // version
        stsd_content.extend_from_slice(&[0, 0, 0]); // flags
        stsd_content.extend_from_slice(&1u32.to_be_bytes()); // entry_count

        // avc1 sample entry
        self.write_avc1(&mut stsd_content);

        let size = 8 + stsd_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsd");
        buf.extend_from_slice(&stsd_content);
    }

    fn write_avc1(&self, buf: &mut Vec<u8>) {
        let mut avc1_content = Vec::new();

        avc1_content.extend_from_slice(&[0; 6]); // reserved
        avc1_content.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index

        avc1_content.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        avc1_content.extend_from_slice(&0u16.to_be_bytes()); // reserved
        avc1_content.extend_from_slice(&[0; 12]); // pre_defined

        avc1_content.extend_from_slice(&(self.width as u16).to_be_bytes());
        avc1_content.extend_from_slice(&(self.height as u16).to_be_bytes());

        avc1_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // horiz resolution 72 dpi
        avc1_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // vert resolution 72 dpi
        avc1_content.extend_from_slice(&0u32.to_be_bytes()); // reserved
        avc1_content.extend_from_slice(&1u16.to_be_bytes()); // frame_count

        // Compressor name (32 bytes)
        let mut compressor = [0u8; 32];
        let name = b"video-toolbox-sys";
        compressor[0] = name.len() as u8;
        compressor[1..1 + name.len()].copy_from_slice(name);
        avc1_content.extend_from_slice(&compressor);

        avc1_content.extend_from_slice(&0x0018u16.to_be_bytes()); // depth (24-bit)
        avc1_content.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined

        // avcC box
        self.write_avcc(&mut avc1_content);

        let size = 8 + avc1_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"avc1");
        buf.extend_from_slice(&avc1_content);
    }

    fn write_avcc(&self, buf: &mut Vec<u8>) {
        let mut avcc_content = Vec::new();

        avcc_content.push(1); // configuration_version

        // Profile, compatibility, and level from SPS
        if self.sps.len() >= 4 {
            avcc_content.push(self.sps[1]); // profile_idc
            avcc_content.push(self.sps[2]); // profile_compatibility
            avcc_content.push(self.sps[3]); // level_idc
        } else {
            avcc_content.extend_from_slice(&[0x64, 0x00, 0x1f]); // High profile, level 3.1
        }

        avcc_content.push(0xFF); // length_size_minus_one (3 = 4 bytes) | reserved (0b111111)

        // SPS
        avcc_content.push(0xE1); // num_sps | reserved (0b111)
        avcc_content.extend_from_slice(&(self.sps.len() as u16).to_be_bytes());
        avcc_content.extend_from_slice(&self.sps);

        // PPS
        avcc_content.push(1); // num_pps
        avcc_content.extend_from_slice(&(self.pps.len() as u16).to_be_bytes());
        avcc_content.extend_from_slice(&self.pps);

        let size = 8 + avcc_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"avcC");
        buf.extend_from_slice(&avcc_content);
    }

    fn write_empty_stts(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // entry_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stts");
        buf.extend_from_slice(&content);
    }

    fn write_empty_stsc(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // entry_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsc");
        buf.extend_from_slice(&content);
    }

    fn write_empty_stsz(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // sample_size
        content.extend_from_slice(&0u32.to_be_bytes()); // sample_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsz");
        buf.extend_from_slice(&content);
    }

    fn write_empty_stco(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // entry_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stco");
        buf.extend_from_slice(&content);
    }

    fn write_mvex(&self, buf: &mut Vec<u8>) {
        let mut mvex_content = Vec::new();

        // trex box
        self.write_trex(&mut mvex_content);

        let size = 8 + mvex_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvex");
        buf.extend_from_slice(&mvex_content);
    }

    fn write_trex(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&self.track_id.to_be_bytes()); // track_id
        content.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
        content.extend_from_slice(&0u32.to_be_bytes()); // default_sample_duration
        content.extend_from_slice(&0u32.to_be_bytes()); // default_sample_size
        content.extend_from_slice(&0u32.to_be_bytes()); // default_sample_flags

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trex");
        buf.extend_from_slice(&content);
    }

    fn write_moof(&self, buf: &mut Vec<u8>) {
        let mut moof_content = Vec::new();

        // mfhd (movie fragment header)
        self.write_mfhd(&mut moof_content);

        // traf (track fragment)
        self.write_traf(&mut moof_content);

        let size = 8 + moof_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"moof");
        buf.extend_from_slice(&moof_content);
    }

    fn write_mfhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&self.sequence_number.to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mfhd");
        buf.extend_from_slice(&content);
    }

    fn write_traf(&self, buf: &mut Vec<u8>) {
        let mut traf_content = Vec::new();

        // tfhd (track fragment header)
        self.write_tfhd(&mut traf_content);

        // tfdt (track fragment decode time)
        self.write_tfdt(&mut traf_content);

        // trun (track run)
        self.write_trun(&mut traf_content, buf.len());

        let size = 8 + traf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"traf");
        buf.extend_from_slice(&traf_content);
    }

    fn write_tfhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        // flags: default-base-is-moof (0x020000)
        content.extend_from_slice(&[0x02, 0x00, 0x00]);
        content.extend_from_slice(&self.track_id.to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tfhd");
        buf.extend_from_slice(&content);
    }

    fn write_tfdt(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(1); // version (1 for 64-bit time)
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&(self.fragment_base_dts as u64).to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tfdt");
        buf.extend_from_slice(&content);
    }

    fn write_trun(&self, buf: &mut Vec<u8>, _moof_offset: usize) {
        let sample_count = self.pending_frames.len() as u32;

        // Calculate trun size to determine data_offset
        // trun header: 8 bytes (size + type)
        // version + flags: 4 bytes
        // sample_count: 4 bytes
        // data_offset: 4 bytes
        // Per sample: duration (4) + size (4) + flags (4) + composition_offset (4) = 16 bytes
        let trun_content_size = 4 + 4 + 4 + (sample_count as usize * 16);
        let trun_size = 8 + trun_content_size;

        // Calculate data_offset from start of moof to start of mdat data
        // moof is at moof_offset in the current buffer
        // After this traf, we write mdat
        // moof_size = current buf len + traf header (8) + tfhd + tfdt + trun
        // Actually we need to compute this differently
        // The data_offset is relative to the start of the moof box
        // We need: moof_size + 8 (mdat header)

        // At this point, buf contains: [styp][moof header][mfhd]
        // We're writing traf which contains: [tfhd][tfdt][trun]
        // Then mdat

        // moof size = 8 + mfhd_size + traf_size
        // traf size = 8 + tfhd_size + tfdt_size + trun_size

        // Let's calculate sizes
        let tfhd_size = 8 + 8; // version/flags + track_id
        let tfdt_size = 8 + 12; // version/flags + 64-bit time
        let traf_size = 8 + tfhd_size + tfdt_size + trun_size;
        let mfhd_size = 8 + 8;
        let moof_size = 8 + mfhd_size + traf_size;

        // data_offset is from start of moof to first byte of mdat data
        // = moof_size + 8 (mdat header)
        let data_offset = moof_size + 8;

        let mut content = Vec::new();

        content.push(0); // version
        // flags: data-offset-present, sample-duration, sample-size, sample-flags, sample-composition-time-offset
        // 0x000001 = data-offset-present
        // 0x000100 = sample-duration-present
        // 0x000200 = sample-size-present
        // 0x000400 = sample-flags-present
        // 0x000800 = sample-composition-time-offsets-present
        content.extend_from_slice(&[0x00, 0x0F, 0x01]); // all flags
        content.extend_from_slice(&sample_count.to_be_bytes());
        content.extend_from_slice(&(data_offset as u32).to_be_bytes());

        for frame in &self.pending_frames {
            content.extend_from_slice(&frame.duration.to_be_bytes());
            content.extend_from_slice(&(frame.data.len() as u32).to_be_bytes());

            // Sample flags
            let flags = if frame.is_sync {
                0x02000000u32 // is_leading=0, depends_on=2 (no other), is_depended_on=0, has_redundancy=0
            } else {
                0x01010000u32 // is_leading=0, depends_on=1 (yes), is_depended_on=1, has_redundancy=0
            };
            content.extend_from_slice(&flags.to_be_bytes());

            content.extend_from_slice(&frame.composition_offset.to_be_bytes());
        }

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trun");
        buf.extend_from_slice(&content);
    }

    fn write_mdat(&self, buf: &mut Vec<u8>) {
        let total_data_size: usize = self.pending_frames.iter().map(|f| f.data.len()).sum();
        let size = 8 + total_data_size;

        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdat");

        for frame in &self.pending_frames {
            buf.extend_from_slice(&frame.data);
        }
    }

    /// Get the current sequence number.
    pub fn sequence_number(&self) -> u32 {
        self.sequence_number
    }

    /// Check if the muxer has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Get the number of pending frames.
    pub fn pending_frame_count(&self) -> usize {
        self.pending_frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CmafConfig::default();
        assert_eq!(config.fragment_duration_ms, 2000);
        assert_eq!(config.timescale, 90000);
    }

    #[test]
    fn test_muxer_initialization() {
        let mut muxer = CmafMuxer::new(CmafConfig::default());
        assert!(!muxer.is_initialized());

        let sps = vec![0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50];
        let pps = vec![0x68, 0xee, 0x3c, 0x80];

        let init = muxer.create_init_segment(&sps, &pps, 1920, 1080);
        assert!(muxer.is_initialized());
        assert!(!init.is_empty());

        // Check ftyp box
        assert_eq!(&init[4..8], b"ftyp");
        // Check moov box exists
        assert!(init.windows(4).any(|w| w == b"moov"));
    }

    #[test]
    fn test_ftyp_box() {
        let muxer = CmafMuxer::new(CmafConfig::default());
        let mut buf = Vec::new();
        muxer.write_ftyp(&mut buf);

        // Verify structure
        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(&buf[4..8], b"ftyp");
        assert_eq!(size as usize, buf.len());
    }
}
