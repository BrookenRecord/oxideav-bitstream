use oxideav_bitstream::av1::sequence::{
    parse_sequence_header_obu, parse_sequence_header_obu_complete,
};

#[test]
fn complete_sequence_preserves_fields_and_reports_syntax_bits() {
    for depth in [8, 10] {
        for ticks in [None, Some(0), Some(1), Some(u32::MAX)] {
            let bytes = fixture(depth, ticks, false);
            let prefix = parse_sequence_header_obu(&bytes).unwrap();
            let (complete, bits) = parse_sequence_header_obu_complete(&bytes).unwrap();
            assert_eq!(complete, prefix);
            assert_eq!(bits, writer_syntax_bits(&bytes));
            assert_eq!(complete.color_config.bit_depth, depth);
            assert_eq!(complete.max_frame_width(), 1920);
            assert_eq!(complete.max_frame_height(), 1080);
            assert_eq!(
                complete
                    .timing_info
                    .and_then(|t| t.num_ticks_per_picture_minus_1),
                ticks,
            );
            assert_eq!(
                complete.operating_points[0].initial_display_delay_minus_1,
                3
            );
            let padded = [bytes, vec![0; 8]].concat();
            let (same, padded_bits) = parse_sequence_header_obu_complete(&padded).unwrap();
            assert_eq!(same, complete);
            assert_eq!(padded_bits, bits);
        }
    }
}

#[test]
fn complete_sequence_rejects_missing_one_and_nonzero_remainder() {
    for (depth, reduced) in [(8, false), (10, false), (8, true)] {
        let bytes = fixture(depth, None, reduced);
        let prefix = parse_sequence_header_obu(&bytes).unwrap();
        let missing = missing_marker(&bytes);
        let nonzero = bad_padding(&bytes);
        // Prefix compatibility remains separate from complete-payload validation.
        assert_eq!(parse_sequence_header_obu(&missing).unwrap(), prefix);
        assert!(matches!(
            parse_sequence_header_obu_complete(&missing),
            Err(oxideav_bitstream::BitstreamError::InvalidData(_))
        ));
        assert!(matches!(
            parse_sequence_header_obu_complete(&nonzero),
            Err(oxideav_bitstream::BitstreamError::InvalidData(_))
        ));
    }
}

#[test]
fn complete_sequence_rejects_every_byte_cut_before_complete_syntax() {
    let bytes = fixture(10, Some(1), false);
    for end in 0..writer_syntax_bits(&bytes).div_ceil(8) {
        assert!(parse_sequence_header_obu_complete(&bytes[..end]).is_err());
    }
}

#[test]
fn corpus_sequence_uses_the_strict_entry_point() {
    let bytes = include_bytes!("fixtures/av1_320x240_1frame.obu");
    let mut seen = false;
    for obu in oxideav_bitstream::av1::iter_obus(bytes) {
        let (header, payload) = obu.unwrap();
        if header.obu_type == oxideav_bitstream::av1::OBU_SEQUENCE_HEADER {
            let prefix = parse_sequence_header_obu(payload).unwrap();
            let (complete, bits) = parse_sequence_header_obu_complete(payload).unwrap();
            assert_eq!(complete, prefix);
            assert!(bits < payload.len() * 8);
            seen = true;
        }
    }
    assert!(seen);
}

use oxideav_bitstream::av1::{
    Av1ColorConfig, Av1SequenceHeader, Av1TimingInfo, write_sequence_header,
};

fn fixture(depth: u8, ticks: Option<u32>, reduced: bool) -> Vec<u8> {
    fixture_dimensions(depth, ticks, reduced, 11, 11)
}

fn fixture_dimensions(
    depth: u8,
    ticks: Option<u32>,
    reduced: bool,
    width_bits: u8,
    height_bits: u8,
) -> Vec<u8> {
    write_sequence_header(&Av1SequenceHeader {
        seq_profile: 0,
        still_picture: reduced,
        reduced_still_picture_header: reduced,
        seq_level_idx_0: 8,
        timing_info: ticks.map(|value| Av1TimingInfo {
            num_units_in_display_tick: 1001,
            time_scale: 60000,
            num_ticks_per_picture_minus_1: Some(value),
        }),
        initial_display_delay_present_flag: !reduced,
        initial_display_delay_minus_1: (!reduced).then_some(3),
        frame_width_bits: width_bits,
        frame_height_bits: height_bits,
        max_frame_width_minus_1: 1919,
        max_frame_height_minus_1: 1079,
        seq_choose_screen_content_tools: true,
        seq_force_screen_content_tools: 2,
        seq_choose_integer_mv: true,
        seq_force_integer_mv: 2,
        enable_cdef: true,
        enable_restoration: true,
        color_config: Av1ColorConfig {
            bit_depth: depth,
            subsampling_x: true,
            subsampling_y: true,
            color_description: Some((1, 1, 1)),
            chroma_sample_position: 1,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap()
}

/// The public writer ends with a one marker and zero padding. This locates that
/// writer-provided marker; it does not parse or duplicate sequence-header syntax.
fn writer_syntax_bits(payload: &[u8]) -> usize {
    payload.len() * 8 - payload.last().unwrap().trailing_zeros() as usize - 1
}

fn missing_marker(payload: &[u8]) -> Vec<u8> {
    let mut bytes = payload.to_vec();
    let last = bytes.last_mut().unwrap();
    *last &= !(1 << last.trailing_zeros());
    bytes
}

fn bad_padding(payload: &[u8]) -> Vec<u8> {
    let mut bytes = payload.to_vec();
    bytes.push(1);
    bytes
}

#[test]
fn every_marker_position_and_every_trailing_bit_are_checked() {
    let mut positions = [false; 8];
    for width in 11..=16 {
        for height in 11..=16 {
            let bytes = fixture_dimensions(8, None, false, width, height);
            let bits = writer_syntax_bits(&bytes);
            positions[bits % 8] = true;
            let (header, actual_bits) = parse_sequence_header_obu_complete(&bytes).unwrap();
            assert_eq!(actual_bits, bits);

            let missing = missing_marker(&bytes);
            assert!(matches!(
                parse_sequence_header_obu_complete(&missing),
                Err(oxideav_bitstream::BitstreamError::InvalidData(_))
            ));
            // When syntax ends at a byte boundary, the entire marker byte can
            // be removed without truncating syntax.
            if bits.is_multiple_of(8) {
                let syntax_only = &bytes[..bits / 8];
                assert_eq!(parse_sequence_header_obu(syntax_only).unwrap(), header);
                assert!(matches!(
                    parse_sequence_header_obu_complete(syntax_only),
                    Err(oxideav_bitstream::BitstreamError::UnexpectedEnd(_))
                ));
            }

            let mut padded = bytes.clone();
            padded.extend_from_slice(&[0; 2]);
            assert_eq!(
                parse_sequence_header_obu_complete(&padded).unwrap(),
                (header, bits)
            );
            for bit in bits + 1..padded.len() * 8 {
                padded[bit / 8] |= 0x80 >> (bit % 8);
                assert!(
                    matches!(
                        parse_sequence_header_obu_complete(&padded),
                        Err(oxideav_bitstream::BitstreamError::InvalidData(_))
                    ),
                    "nonzero trailing bit {bit}, syntax bits {bits}"
                );
                padded[bit / 8] &= !(0x80 >> (bit % 8));
            }
        }
    }
    assert_eq!(positions, [true; 8]);
}

#[test]
fn prefix_entry_points_keep_their_existing_tail_behavior() {
    for (depth, reduced) in [(8, false), (10, false), (8, true)] {
        let bytes = fixture(depth, None, reduced);
        let modern = parse_sequence_header_obu(&bytes).unwrap();
        let legacy = oxideav_bitstream::av1::parse_sequence_header(&bytes).unwrap();
        for changed in [missing_marker(&bytes), bad_padding(&bytes)] {
            assert_eq!(parse_sequence_header_obu(&changed).unwrap(), modern);
            assert_eq!(
                oxideav_bitstream::av1::parse_sequence_header(&changed).unwrap(),
                legacy
            );
        }
    }
}

#[test]
fn exact_malformed_sequence_payloads_reject() {
    // Public-writer outputs with the mandatory trailing marker cleared.
    let payloads: &[&[u8]] = &[
        &[
            0x04, 0x00, 0x00, 0x0f, 0xa4, 0x00, 0x03, 0xa9, 0x83, 0x40, 0x00, 0x08, 0x4e, 0xab,
            0xbf, 0xc3, 0x70, 0x06, 0xc8, 0x08, 0x08, 0x09, 0x00,
        ],
        &[
            0x02, 0x00, 0x00, 0x42, 0x75, 0x5d, 0xfe, 0x1b, 0x80, 0x37, 0x40, 0x40, 0x40, 0x48,
        ],
        &[0x1a, 0x2a, 0xbb, 0xfc, 0x37, 0x0c, 0x80, 0x80, 0x80, 0x90],
    ];
    for payload in payloads {
        assert!(parse_sequence_header_obu(payload).is_ok());
        assert!(matches!(
            parse_sequence_header_obu_complete(payload),
            Err(oxideav_bitstream::BitstreamError::InvalidData(_))
        ));
    }
}
