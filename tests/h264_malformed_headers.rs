use oxideav_bitstream::BitstreamError;
use oxideav_bitstream::bit_writer::BitWriter;
use oxideav_bitstream::h264::slice::{SliceHeader, parse_slice_header_nal};
use oxideav_bitstream::h264::stream::{H264Event, H264Parser};
use oxideav_bitstream::h264::{self, H264Pps, H264Sps};
use oxideav_bitstream::nal::rbsp_to_ebsp;

fn nal(kind: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = vec![0x60 | kind];
    out.extend(rbsp_to_ebsp(rbsp));
    out
}

// Author raw syntax so malformed values cannot be normalized by a writer.
fn sps_rbsp(overrides: &[(&str, u32)]) -> Vec<u8> {
    let value = |name, default| {
        overrides
            .iter()
            .find(|(key, _)| *key == name)
            .map_or(default, |(_, value)| *value)
    };
    let mut w = BitWriter::new();
    w.write_bits(100, 8);
    w.write_bits(0, 8);
    w.write_bits(30, 8);
    for (field, default) in [
        ("id", 0),
        ("chroma", 1),
        ("luma_depth", 0),
        ("chroma_depth", 0),
    ] {
        w.write_ue(value(field, default)).unwrap();
    }
    w.write_bit(0); // qpprime_y_zero_transform_bypass_flag
    w.write_bit(0); // seq_scaling_matrix_present_flag
    w.write_ue(0).unwrap(); // log2_max_frame_num_minus4
    w.write_ue(value("poc_type", 2)).unwrap();
    w.write_ue(1).unwrap(); // max_num_ref_frames
    w.write_bit(0); // gaps_in_frame_num_value_allowed_flag
    w.write_ue(value("width", 19)).unwrap();
    w.write_ue(value("height", 14)).unwrap();
    w.write_bit(1); // frame_mbs_only_flag
    w.write_bit(1); // direct_8x8_inference_flag
    let cropped = overrides.iter().any(|(name, _)| name.starts_with("crop_"));
    w.write_bit(u32::from(cropped));
    if cropped {
        for name in ["crop_left", "crop_right", "crop_top", "crop_bottom"] {
            w.write_ue(value(name, 0)).unwrap();
        }
    }
    w.write_bit(0); // vui_parameters_present_flag
    w.write_rbsp_trailing_bits();
    w.finish()
}

fn pps_rbsp(ids: [u32; 2], refs: [u32; 2]) -> Vec<u8> {
    let mut w = BitWriter::new();
    for id in ids {
        w.write_ue(id).unwrap();
    }
    w.write_bits(0, 2); // entropy_coding / bottom_field_pic_order flags
    w.write_ue(0).unwrap(); // num_slice_groups_minus1
    for count in refs {
        w.write_ue(count).unwrap();
    }
    w.write_bits(0, 3); // weighted prediction flags
    for _ in 0..3 {
        w.write_se(0).unwrap();
    }
    w.write_bits(0, 3); // deblock, constrained, redundant flags
    w.write_rbsp_trailing_bits();
    w.finish()
}

fn idr(qp_delta: i32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_ue(0).unwrap(); // first_mb_in_slice
    w.write_ue(2).unwrap(); // I slice
    w.write_ue(0).unwrap(); // PPS id
    w.write_bits(0, 4); // frame_num
    w.write_ue(0).unwrap(); // idr_pic_id
    w.write_bits(0, 2); // dec_ref_pic_marking
    w.write_se(qp_delta).unwrap();
    w.write_rbsp_trailing_bits();
    nal(5, &w.finish())
}

#[test]
fn sps_rejects_values_that_would_alias_when_narrowed() {
    for (name, value) in [
        ("id", 256),
        ("chroma", 257),
        ("luma_depth", 256),
        ("chroma_depth", 256),
        ("poc_type", 258),
        ("id", 32),
        ("chroma", 4),
        ("luma_depth", 7),
        ("chroma_depth", 7),
    ] {
        let bytes = sps_rbsp(&[(name, value)]);
        assert!(
            matches!(h264::parse_sps(&bytes), Err(BitstreamError::InvalidData(_))),
            "{name}={value}"
        );
        let mut parser = H264Parser::new();
        assert!(parser.process_nal(&nal(7, &bytes)).is_err());
        assert!(parser.sps(0).is_none());
    }
}

#[test]
fn pps_rejects_ids_and_reference_counts_before_narrowing() {
    for (ids, refs) in [
        ([256, 0], [0, 0]),
        ([0, 256], [0, 0]),
        ([0, 32], [0, 0]),
        ([0, 0], [256, 0]),
        ([0, 0], [0, 256]),
        ([0, 0], [32, 0]),
    ] {
        assert!(matches!(
            h264::parse_pps(&pps_rbsp(ids, refs)),
            Err(BitstreamError::InvalidData(_))
        ));
    }
    assert_eq!(
        h264::parse_pps(&pps_rbsp([255, 31], [31, 31]))
            .unwrap()
            .pic_parameter_set_id,
        255
    );
}

#[test]
fn sps_dimensions_and_crops_are_checked_before_exposing_helpers() {
    for fields in [
        vec![("width", u32::MAX - 1)],
        vec![("height", u32::MAX - 1)],
        vec![("crop_left", u32::MAX - 1), ("crop_right", u32::MAX - 1)],
        vec![("crop_top", u32::MAX - 1), ("crop_bottom", u32::MAX - 1)],
    ] {
        assert!(matches!(
            h264::parse_sps(&sps_rbsp(&fields)),
            Err(BitstreamError::InvalidData(_))
        ));
    }
    let sps = h264::parse_sps(&sps_rbsp(&[("crop_left", 1), ("crop_top", 1)])).unwrap();
    assert_eq!((sps.display_width(), sps.display_height()), (318, 238));
}

#[test]
fn parameter_sets_require_complete_rbsp_tails() {
    for (kind, valid) in [(7, sps_rbsp(&[])), (8, pps_rbsp([0, 0], [0, 0]))] {
        let mut parser = H264Parser::new();
        assert!(parser.process_nal(&nal(kind, &valid)).is_ok());
        for end in 0..valid.len() {
            assert!(
                parser.process_nal(&nal(kind, &valid[..end])).is_err(),
                "kind={kind} end={end}"
            );
        }
        let mut bad = valid.clone();
        *bad.last_mut().unwrap() = 0;
        assert!(parser.process_nal(&nal(kind, &bad)).is_err());
        let mut bad = valid.clone();
        bad.extend([0x80]);
        assert!(parser.process_nal(&nal(kind, &bad)).is_err());
    }
    // The old positive max-frame-num fixture stopped after poc_type.
    let mut w = BitWriter::new();
    w.write_bits(66, 8);
    w.write_bits(0, 16);
    w.write_ue(0).unwrap();
    w.write_ue(12).unwrap();
    w.write_ue(2).unwrap();
    assert!(h264::parse_sps(&w.finish()).is_err());
}

#[test]
fn extreme_qp_delta_is_rejected_without_overflow() {
    let sps = h264::parse_sps(&sps_rbsp(&[])).unwrap();
    let pps = H264Pps::default();
    assert!(parse_slice_header_nal(&idr(0), &sps, &pps).is_ok());
    assert!(matches!(
        parse_slice_header_nal(&idr(i32::MAX), &sps, &pps),
        Err(BitstreamError::InvalidData(_))
    ));
    let mut header = SliceHeader::default();
    header.slice_qp_delta = i32::MAX;
    assert_eq!(header.slice_qp_y(&pps), i32::MAX);
}

#[test]
fn fixed_width_slice_prefixes_cannot_succeed_after_overread() {
    for fixture in [
        &include_bytes!("fixtures/h264_baseline_320x240_1frame.h264")[..],
        &include_bytes!("fixtures/h264_high_320x240_1frame.h264")[..],
    ] {
        let mut parser = H264Parser::new();
        for bytes in h264::stream::nal_units(fixture) {
            if let H264Event::Slice(slice) = parser.process_nal(bytes).unwrap() {
                let (sps, pps) = parser.active(&slice).unwrap();
                let needed = slice.slice_data_bit_offset.div_ceil(8);
                for end in 1..needed {
                    assert!(
                        parse_slice_header_nal(&bytes[..end], sps, pps).is_err(),
                        "end={end} needed={needed}"
                    );
                }
            }
        }
    }
    let sps = H264Sps {
        frame_mbs_only_flag: true,
        ..H264Sps::default()
    };
    assert!(h264::parse_slice_header_minimal(&[0xb8], 1, &sps, &H264Pps::default()).is_err());
}
