use oxideav_bitstream::BitstreamError;
use oxideav_bitstream::bit_writer::BitWriter;
use oxideav_bitstream::hevc::slice::{SliceSegmentHeader, parse_slice_segment_header};
use oxideav_bitstream::hevc::stream::{HevcEvent, HevcParser};
use oxideav_bitstream::hevc::{self, HevcPps, HevcSps};
use oxideav_bitstream::nal::rbsp_to_ebsp;

fn nal(kind: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = vec![kind << 1, 1];
    out.extend(rbsp_to_ebsp(rbsp));
    out
}

fn sps_rbsp(overrides: &[(&str, u32)], predicted_rps: bool) -> Vec<u8> {
    let value = |name, default| {
        overrides
            .iter()
            .find(|(key, _)| *key == name)
            .map_or(default, |(_, value)| *value)
    };
    let mut w = BitWriter::new();
    w.write_bits(0, 4); // VPS id
    w.write_bits(0, 3); // max_sub_layers_minus1
    w.write_bit(1); // temporal_id_nesting
    w.write_bits(1, 8); // profile_space, tier, Main profile
    w.write_bits(0x6000_0000, 32); // compatibility
    w.write_bits(0, 32); // constraints
    w.write_bits(0, 16); // constraints
    w.write_bits(63, 8); // level
    w.write_ue(value("id", 0)).unwrap();
    w.write_ue(value("chroma", 1)).unwrap();
    w.write_ue(value("width", 320)).unwrap();
    w.write_ue(value("height", 240)).unwrap();
    let cropped = overrides.iter().any(|(name, _)| name.starts_with("crop_"));
    w.write_bit(u32::from(cropped));
    if cropped {
        for name in ["crop_left", "crop_right", "crop_top", "crop_bottom"] {
            w.write_ue(value(name, 0)).unwrap();
        }
    }
    w.write_ue(value("luma_depth", 0)).unwrap();
    w.write_ue(value("chroma_depth", 0)).unwrap();
    w.write_ue(0).unwrap(); // log2_max_poc_lsb_minus4
    w.write_bit(0); // sub_layer_ordering_info_present_flag
    w.write_ue(value("dpb", 4)).unwrap();
    w.write_ue(0).unwrap(); // max_num_reorder_pics
    w.write_ue(0).unwrap(); // max_latency_increase_plus1
    w.write_ue(value("min_cb", 0)).unwrap();
    w.write_ue(value("diff_cb", 3)).unwrap();
    for (name, default) in [
        ("min_tb", 0),
        ("diff_tb", 3),
        ("depth_inter", 0),
        ("depth_intra", 0),
    ] {
        w.write_ue(value(name, default)).unwrap();
    }
    w.write_bits(0, 3); // scaling_list, amp, sao flags
    let pcm = overrides.iter().any(|(name, _)| name.starts_with("pcm_"));
    w.write_bit(u32::from(pcm));
    if pcm {
        w.write_bits(7, 4);
        w.write_bits(7, 4);
        w.write_ue(value("pcm_min", 0)).unwrap();
        w.write_ue(value("pcm_diff", 0)).unwrap();
        w.write_bit(0);
    }
    w.write_ue(if predicted_rps { 2 } else { 0 }).unwrap();
    if predicted_rps {
        w.write_ue(1).unwrap(); // one negative picture in first RPS
        w.write_ue(0).unwrap(); // no positive pictures
        w.write_ue(0).unwrap(); // delta POC = -1
        w.write_bit(1); // used by current
        w.write_bit(1); // next RPS predicts previous
        w.write_bit(1); // delta RPS is negative
        w.write_ue(0).unwrap(); // delta RPS = -1
        w.write_bits(3, 2); // retain prior picture and delta RPS picture
    }
    let lt = value("lt_sps", 0);
    w.write_bit(u32::from(lt != 0));
    if lt != 0 {
        w.write_ue(lt).unwrap();
        for _ in 0..lt {
            w.write_bits(0, 5);
        }
    }
    w.write_bits(0, 3); // temporal MVP, smoothing, VUI
    let extension = value("extension", 0);
    w.write_bit(u32::from(extension != 0));
    if extension != 0 {
        w.write_bits(0, 4); // known extension flags
        w.write_bits(extension, 4);
        w.write_bits(0, 12); // opaque zero-valued extension data
    }
    w.write_rbsp_trailing_bits();
    w.finish()
}

fn sps(overrides: &[(&str, u32)]) -> HevcSps {
    hevc::parse_sps_nal(&nal(33, &sps_rbsp(overrides, false))).unwrap()
}

fn pps_rbsp(ids: [u32; 2], refs: [u32; 2]) -> Vec<u8> {
    let mut w = BitWriter::new();
    for id in ids {
        w.write_ue(id).unwrap();
    }
    w.write_bits(0, 7); // dependent, output, extra(3), sign, cabac flags
    for count in refs {
        w.write_ue(count).unwrap();
    }
    w.write_se(0).unwrap(); // init QP
    w.write_bits(0, 3); // constrained intra, transform skip, cu QP flags
    w.write_se(0).unwrap();
    w.write_se(0).unwrap(); // chroma QP
    w.write_bits(0, 8); // chroma offsets, weighted(2), bypass, tiles, WPP, loop, deblock
    w.write_bits(0, 2); // scaling list, list modification
    w.write_ue(0).unwrap(); // parallel merge level
    w.write_bits(0, 2); // slice header extension, PPS extension
    w.write_rbsp_trailing_bits();
    w.finish()
}

fn idr(qp_delta: i32, invalid_alignment: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bit(1); // first slice
    w.write_bit(0); // no_output_of_prior_pics
    w.write_ue(0).unwrap(); // PPS id
    w.write_ue(2).unwrap(); // I slice
    w.write_se(qp_delta).unwrap();
    w.write_bit(1); // byte_alignment one
    if invalid_alignment {
        w.write_bit(1);
    }
    w.align_to_byte();
    nal(19, &w.finish())
}

#[test]
fn sps_rejects_narrowed_ids_chroma_depth_transform_and_pcm_fields() {
    for (field, value) in [
        ("id", 256),
        ("chroma", 257),
        ("luma_depth", 256),
        ("chroma_depth", 256),
        ("min_tb", 256),
        ("diff_tb", 256),
        ("depth_inter", 256),
        ("depth_intra", 256),
        ("pcm_min", 256),
        ("pcm_diff", 256),
        ("id", 16),
        ("chroma", 4),
        ("luma_depth", 9),
        ("chroma_depth", 9),
    ] {
        let bytes = nal(33, &sps_rbsp(&[(field, value)], false));
        assert!(
            matches!(
                hevc::parse_sps_nal(&bytes),
                Err(BitstreamError::InvalidData(_))
            ),
            "{field}={value}"
        );
        let mut parser = HevcParser::new();
        assert!(parser.process_nal(&bytes).is_err());
        assert!(parser.sps(0).is_none());
    }
    assert_eq!(
        sps(&[("luma_depth", 2), ("chroma_depth", 2)]).bit_depth_luma_minus8,
        2
    );
}

#[test]
fn pps_rejects_narrowed_ids_and_reference_counts() {
    for (ids, refs) in [
        ([256, 0], [0, 0]),
        ([0, 256], [0, 0]),
        ([64, 0], [0, 0]),
        ([0, 16], [0, 0]),
        ([0, 0], [256, 0]),
        ([0, 0], [0, 256]),
        ([0, 0], [15, 0]),
    ] {
        assert!(matches!(
            hevc::parse_pps_nal(&nal(34, &pps_rbsp(ids, refs))),
            Err(BitstreamError::InvalidData(_))
        ));
    }
    assert_eq!(
        hevc::parse_pps_nal(&nal(34, &pps_rbsp([63, 15], [14, 14])))
            .unwrap()
            .pps_pic_parameter_set_id,
        63
    );
}

#[test]
fn sps_rejects_overflowing_conformance_windows() {
    for fields in [
        vec![("width", 0)],
        vec![("height", 0)],
        vec![("crop_left", u32::MAX - 1), ("crop_right", u32::MAX - 1)],
        vec![("crop_top", u32::MAX - 1), ("crop_bottom", u32::MAX - 1)],
    ] {
        assert!(matches!(
            hevc::parse_sps_nal(&nal(33, &sps_rbsp(&fields, false))),
            Err(BitstreamError::InvalidData(_))
        ));
    }
    let sps = sps(&[("crop_left", 1), ("crop_top", 1)]);
    assert_eq!((sps.display_width(), sps.display_height()), (318, 238));
}

#[test]
fn predicted_rps_cannot_grow_beyond_the_sps_dpb_limit() {
    let valid = nal(33, &sps_rbsp(&[("dpb", 2)], true));
    assert_eq!(
        hevc::parse_sps_nal(&valid).unwrap().short_term_rps[1].num_delta_pocs(),
        2
    );
    let invalid = nal(33, &sps_rbsp(&[("dpb", 1)], true));
    assert!(matches!(
        hevc::parse_sps_nal(&invalid),
        Err(BitstreamError::InvalidData(_))
    ));
}

#[test]
fn long_term_count_addition_cannot_wrap_to_an_empty_loop() {
    let sps = sps(&[("lt_sps", 2)]);
    let mut w = BitWriter::new();
    w.write_bit(1); // first slice, non-IRAP
    w.write_ue(0).unwrap(); // PPS
    w.write_ue(2).unwrap(); // I slice
    w.write_bits(0, 4); // POC
    w.write_bit(0); // explicit short-term RPS
    w.write_ue(0).unwrap();
    w.write_ue(0).unwrap(); // no short-term refs
    w.write_ue(2).unwrap(); // two SPS long-term refs
    w.write_ue(u32::MAX - 1).unwrap(); // overflows when added to two
    w.write_se(0).unwrap(); // QP if the count incorrectly wraps
    w.write_bit(1);
    w.align_to_byte();
    assert!(matches!(
        parse_slice_segment_header(&nal(1, &w.finish()), &sps, &HevcPps::default(), None),
        Err(BitstreamError::InvalidData(_))
    ));
}

#[test]
fn qp_extremes_are_rejected_without_overflow() {
    let sps = sps(&[]);
    let pps = HevcPps::default();
    assert!(parse_slice_segment_header(&idr(0, false), &sps, &pps, None).is_ok());
    assert!(matches!(
        parse_slice_segment_header(&idr(i32::MAX, false), &sps, &pps, None),
        Err(BitstreamError::InvalidData(_))
    ));
    let mut header = SliceSegmentHeader::default();
    header.slice_qp_delta = i32::MAX;
    assert_eq!(header.slice_qp_y(&pps), i32::MAX);
}

#[test]
fn weight_denominator_extreme_is_rejected_without_overflow() {
    let sps = sps(&[]);
    let pps = HevcPps {
        weighted_pred_flag: true,
        ..HevcPps::default()
    };
    let mut w = BitWriter::new();
    w.write_bit(1);
    w.write_ue(0).unwrap();
    w.write_ue(1).unwrap(); // first, PPS, P slice
    w.write_bits(0, 4);
    w.write_bit(0); // POC, explicit short-term RPS
    w.write_ue(0).unwrap();
    w.write_ue(0).unwrap(); // empty RPS
    w.write_bit(0); // no active-reference override
    w.write_ue(1).unwrap(); // luma denominator
    w.write_se(i32::MAX).unwrap(); // chroma denominator delta
    assert!(matches!(
        parse_slice_segment_header(&nal(1, &w.finish()), &sps, &pps, None),
        Err(BitstreamError::InvalidData(_))
    ));
}

#[test]
fn byte_alignment_requires_zero_padding() {
    let sps = sps(&[]);
    assert!(matches!(
        parse_slice_segment_header(&idr(1, true), &sps, &HevcPps::default(), None),
        Err(BitstreamError::InvalidData(_))
    ));
}

#[test]
fn parameter_sets_require_complete_rbsp_tails() {
    let fixture = include_bytes!("fixtures/hevc_main_320x240_1frame.h265");
    for bytes in hevc::stream::nal_units(fixture) {
        let kind = (bytes[0] >> 1) & 63;
        if !(32..=34).contains(&kind) {
            continue;
        }
        let rbsp = hevc::ebsp_to_rbsp(&bytes[2..]);
        for end in 0..rbsp.len() {
            let mut parser = HevcParser::new();
            assert!(
                parser.process_nal(&nal(kind, &rbsp[..end])).is_err(),
                "kind={kind} end={end}"
            );
        }
        let mut bad = rbsp.clone();
        *bad.last_mut().unwrap() = 0;
        assert!(HevcParser::new().process_nal(&nal(kind, &bad)).is_err());
        let mut bad = rbsp;
        bad.push(0x80);
        assert!(HevcParser::new().process_nal(&nal(kind, &bad)).is_err());
    }
}

#[test]
fn fixed_width_slice_prefixes_cannot_succeed_after_overread() {
    for fixture in [
        &include_bytes!("fixtures/hevc_main_320x240_1frame.h265")[..],
        &include_bytes!("fixtures/hevc/main10.h265")[..],
    ] {
        let mut parser = HevcParser::new();
        for bytes in hevc::stream::nal_units(fixture) {
            if let HevcEvent::Slice(slice) = parser.process_nal(bytes).unwrap() {
                let (sps, pps) = parser.active(&slice).unwrap();
                let needed = slice.header.slice_data_byte_offset();
                for end in 2..needed {
                    assert!(
                        parse_slice_segment_header(&bytes[..end], sps, pps, None).is_err(),
                        "end={end} needed={needed}"
                    );
                }
            }
        }
    }
}

#[test]
fn unknown_extension_data_still_requires_a_final_marker() {
    let valid = sps_rbsp(&[("extension", 1)], false);
    assert_eq!(
        hevc::parse_sps_nal(&nal(33, &valid))
            .unwrap()
            .sps_extension_4bits,
        1
    );
    let mut no_marker = valid.clone();
    *no_marker.last_mut().unwrap() = 0;
    assert!(hevc::parse_sps_nal(&nal(33, &no_marker)).is_err());
    let mut extra_zero_byte = valid;
    extra_zero_byte.push(0);
    assert!(hevc::parse_sps_nal(&nal(33, &extra_zero_byte)).is_err());
}

#[test]
fn minimal_slice_fixed_width_overread_is_rejected() {
    let mut sps = sps(&[]);
    sps.log2_max_pic_order_cnt_lsb_minus4 = 12;
    // First slice, PPS 0, I slice; no room for the required 16-bit POC.
    assert!(matches!(
        hevc::parse_slice_header_minimal(&nal(1, &[0xd8]), &sps, &HevcPps::default()),
        Err(BitstreamError::UnexpectedEnd(_))
    ));
}

#[test]
fn transform_bounds_are_checked_before_subtracting_from_the_maximum() {
    let rbsp = sps_rbsp(&[("min_cb", 3), ("diff_cb", 0), ("min_tb", 4)], false);
    assert!(matches!(
        hevc::parse_sps_nal(&nal(33, &rbsp)),
        Err(BitstreamError::InvalidData(_))
    ));
}

fn non_first_idr(address: u64, address_bits: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bit(0); // first_slice_segment_in_pic_flag
    w.write_bit(0); // no_output_of_prior_pics_flag
    w.write_ue(0).unwrap(); // PPS id
    w.write_bits_u64(address, address_bits);
    w.write_ue(2).unwrap(); // I slice
    w.write_se(0).unwrap(); // QP delta
    w.write_bit(1); // byte_alignment one
    w.align_to_byte();
    nal(19, &w.finish())
}

#[test]
fn non_first_slice_full_parser_rejects_an_address_wider_than_u32() {
    let sps = sps(&[("width", 1 << 23), ("height", 1 << 23)]);
    let pps = hevc::parse_pps_nal(&nal(34, &pps_rbsp([0, 0], [0, 0]))).unwrap();
    // 2^34 CTBs require 34 bits; the old release parser truncated this to 1.
    let bytes = non_first_idr((1 << 33) + 1, 34);
    assert!(matches!(
        parse_slice_segment_header(&bytes, &sps, &pps, None),
        Err(BitstreamError::Unsupported(_))
    ));
}

#[test]
fn non_first_slice_minimal_parser_rejects_an_address_wider_than_u32() {
    let sps = sps(&[("width", 1 << 23), ("height", 1 << 23)]);
    let pps = hevc::parse_pps_nal(&nal(34, &pps_rbsp([0, 0], [0, 0]))).unwrap();
    let bytes = non_first_idr((1 << 33) + 1, 34);
    assert!(matches!(
        hevc::parse_slice_header_minimal(&bytes, &sps, &pps),
        Err(BitstreamError::Unsupported(_))
    ));
}

#[test]
fn non_first_slice_in_a_two_ctb_picture_reads_exactly_one_address_bit() {
    let sps = sps(&[("width", 128), ("height", 64)]);
    let pps = hevc::parse_pps_nal(&nal(34, &pps_rbsp([0, 0], [0, 0]))).unwrap();
    let bytes = non_first_idr(1, 1);
    let full = parse_slice_segment_header(&bytes, &sps, &pps, None).unwrap();
    assert_eq!(full.slice_segment_address, 1);
    assert_eq!(full.slice_type, hevc::slice::HevcSliceType::I);
    let minimal = hevc::parse_slice_header_minimal(&bytes, &sps, &pps).unwrap();
    assert_eq!(minimal.slice_type, 2);
}

#[test]
fn non_first_slice_accepts_the_largest_supported_address_width() {
    let sps = sps(&[("width", 1 << 22), ("height", 1 << 22)]);
    let pps = hevc::parse_pps_nal(&nal(34, &pps_rbsp([0, 0], [0, 0]))).unwrap();
    let bytes = non_first_idr(u64::from(u32::MAX), 32);
    let full = parse_slice_segment_header(&bytes, &sps, &pps, None).unwrap();
    assert_eq!(full.slice_segment_address, u32::MAX);
    assert_eq!(full.slice_type, hevc::slice::HevcSliceType::I);
    let minimal = hevc::parse_slice_header_minimal(&bytes, &sps, &pps).unwrap();
    assert_eq!(minimal.slice_type, 2);
}
