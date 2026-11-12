use oxideav_bitstream::BitstreamError;
use oxideav_bitstream::bit_reader::BitReader;

#[test]
fn exp_golomb_requires_both_terminator_and_suffix() {
    for bytes in [&[][..], &[0][..], &[0, 0, 0][..], &[1][..]] {
        let mut reader = BitReader::new(bytes);
        assert!(matches!(reader.ue(), Err(BitstreamError::UnexpectedEnd(_))));
        assert!(reader.bit_pos() <= reader.total_bits());
        let mut reader = BitReader::new(bytes);
        assert!(matches!(reader.se(), Err(BitstreamError::UnexpectedEnd(_))));
    }
    let mut reader = BitReader::new(&[0, 0, 0, 0]);
    assert!(matches!(reader.ue(), Err(BitstreamError::InvalidData(_))));
}

#[test]
fn fixed_width_zero_padding_is_unchanged() {
    let mut reader = BitReader::new(&[0x80]);
    assert_eq!(reader.u(16), 0x8000);
    assert_eq!(reader.bit_pos(), 16);
    assert_eq!(reader.u64(64), 0);
    assert_eq!(reader.read_bit(), 0);
}

#[test]
fn zero_valued_rbsp_data_precedes_the_trailing_marker() {
    for zero_bits in 1..8 {
        let bytes = [0x80 >> zero_bits];
        let mut reader = BitReader::new(&bytes);
        for _ in 0..zero_bits {
            assert!(reader.more_rbsp_data());
            assert_eq!(reader.read_bit(), 0);
        }
        assert!(!reader.more_rbsp_data());
        reader.read_rbsp_trailing_bits().unwrap();
        assert!(reader.at_end());
    }
}
