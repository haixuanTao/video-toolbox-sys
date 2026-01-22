//! FourCC codec constants for video, audio, and pixel formats.
//!
//! These constants are commonly used when working with VideoToolbox,
//! CoreMedia, and CoreVideo frameworks.

/// Video codec FourCC constants (CMVideoCodecType)
pub mod video {
    /// H.264/AVC codec ('avc1')
    pub const H264: u32 = 0x61766331;

    /// HEVC/H.265 codec ('hvc1')
    pub const HEVC: u32 = 0x68766331;

    /// MPEG-4 Video codec ('mp4v')
    pub const MPEG4: u32 = 0x6d703476;

    /// Apple ProRes 422 ('apcn')
    pub const PRORES_422: u32 = 0x6170636e;

    /// Apple ProRes 4444 ('ap4h')
    pub const PRORES_4444: u32 = 0x61703468;

    /// JPEG ('jpeg')
    pub const JPEG: u32 = 0x6a706567;
}

/// Pixel format FourCC constants (CVPixelFormatType)
pub mod pixel {
    /// 32-bit BGRA ('BGRA')
    pub const BGRA32: u32 = 0x42475241;

    /// 32-bit ARGB ('ARGB')
    pub const ARGB32: u32 = 0x00000020;

    /// 32-bit RGBA
    pub const RGBA32: u32 = 0x52474241;

    /// Bi-Planar Y'CbCr 4:2:0 ('420v')
    pub const YUV420_BIPLANAR_VIDEO_RANGE: u32 = 0x34323076;

    /// Bi-Planar Y'CbCr 4:2:0 full range ('420f')
    pub const YUV420_BIPLANAR_FULL_RANGE: u32 = 0x34323066;

    /// Planar Y'CbCr 4:2:2 ('y422')
    pub const YUV422: u32 = 0x79343232;

    /// 24-bit RGB
    pub const RGB24: u32 = 0x00000018;
}

/// Audio codec FourCC constants (AudioFormatID)
pub mod audio {
    /// MPEG-4 AAC ('aac ')
    pub const AAC: u32 = 0x61616320;

    /// Linear PCM
    pub const LPCM: u32 = 0x6c70636d;

    /// Apple Lossless ('alac')
    pub const ALAC: u32 = 0x616c6163;

    /// MPEG Layer 3 ('.mp3')
    pub const MP3: u32 = 0x2e6d7033;

    /// Opus ('opus')
    pub const OPUS: u32 = 0x6f707573;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_codecs() {
        // Verify FourCC byte order (big-endian)
        assert_eq!(video::H264, u32::from_be_bytes(*b"avc1"));
        assert_eq!(video::HEVC, u32::from_be_bytes(*b"hvc1"));
        assert_eq!(video::MPEG4, u32::from_be_bytes(*b"mp4v"));
    }

    #[test]
    fn test_pixel_formats() {
        assert_eq!(pixel::BGRA32, u32::from_be_bytes(*b"BGRA"));
    }

    #[test]
    fn test_audio_codecs() {
        assert_eq!(audio::AAC, u32::from_be_bytes(*b"aac "));
    }
}
