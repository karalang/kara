//! `std.protobuf` — proto3 wire format (slice 1: the low-level codec).
//!
//! Exercises the pure-Kāra wire primitives (`ProtoBuf` encoders +
//! `ProtoReader` cursor): varint, ZigZag, field tags, length-delimited
//! strings/bytes, and fixed widths, each round-tripped encode→decode.
//! Spec: phase-8-stdlib-floor.md § protobuf (wire format).

fn run(src: &str) -> Vec<String> {
    karac::run_program(src)
}

#[test]
fn varint_encodes_canonically() {
    // 300 = 0b1_0010_1100 → low 7 bits 0101100 with continuation (172), then 2.
    let src = "
fn main() {
    let e = ProtoBuf.encode_varint(300u64);
    println(e.len());
    println(e[0]);
    println(e[1]);
}";
    assert_eq!(run(src), vec!["2\n", "172\n", "2\n"]);
}

#[test]
fn varint_round_trips() {
    let src = "
fn main() {
    let vals = Array[0u64, 1u64, 127u64, 128u64, 300u64, 16384u64, 4294967295u64];
    let mut i = 0;
    while i < vals.len() {
        let mut r = ProtoReader.new(ProtoBuf.encode_varint(vals[i]));
        println(r.read_varint());
        i = i + 1;
    }
}";
    assert_eq!(
        run(src),
        vec![
            "0\n",
            "1\n",
            "127\n",
            "128\n",
            "300\n",
            "16384\n",
            "4294967295\n"
        ]
    );
}

#[test]
fn single_byte_varint_advances_one() {
    let src = "
fn main() {
    let mut r = ProtoReader.new(ProtoBuf.encode_varint(5u64));
    println(r.read_varint());
    println(r.at_end());
}";
    assert_eq!(run(src), vec!["5\n", "true\n"]);
}

#[test]
fn zigzag_maps_small_negatives_small() {
    let src = "
fn main() {
    println(ProtoBuf.zigzag_encode(0));
    println(ProtoBuf.zigzag_encode(-1));
    println(ProtoBuf.zigzag_encode(1));
    println(ProtoBuf.zigzag_encode(-2));
    println(ProtoBuf.zigzag_encode(2147483647));
}";
    assert_eq!(run(src), vec!["0\n", "1\n", "2\n", "3\n", "4294967294\n"]);
}

#[test]
fn zigzag_round_trips_through_varint() {
    let src = "
fn main() {
    let vals = Array[0, -1, 1, -150, 150, -2147483648, 2147483647];
    let mut i = 0;
    while i < vals.len() {
        let enc = ProtoBuf.encode_varint(ProtoBuf.zigzag_encode(vals[i]));
        let mut r = ProtoReader.new(enc);
        println(r.read_zigzag());
        i = i + 1;
    }
}";
    assert_eq!(
        run(src),
        vec![
            "0\n",
            "-1\n",
            "1\n",
            "-150\n",
            "150\n",
            "-2147483648\n",
            "2147483647\n"
        ]
    );
}

#[test]
fn tag_round_trips_field_and_wire_type() {
    let src = "
fn main() {
    let t = ProtoBuf.encode_tag(5, ProtoBuf.wire_len_delim());
    let mut r = ProtoReader.new(t);
    let (field, wt) = r.read_tag();
    println(field);
    println(wt);
}";
    assert_eq!(run(src), vec!["5\n", "2\n"]);
}

#[test]
fn high_field_number_tag_round_trips() {
    // Field 1000 needs a multi-byte tag varint.
    let src = "
fn main() {
    let mut r = ProtoReader.new(ProtoBuf.encode_tag(1000, ProtoBuf.wire_varint()));
    let (field, wt) = r.read_tag();
    println(field);
    println(wt);
}";
    assert_eq!(run(src), vec!["1000\n", "0\n"]);
}

#[test]
fn string_field_round_trips() {
    let src = "
fn main() {
    let mut r = ProtoReader.new(ProtoBuf.encode_string(\"hello protobuf\"));
    println(r.read_string());
    println(r.at_end());
}";
    assert_eq!(run(src), vec!["hello protobuf\n", "true\n"]);
}

#[test]
fn empty_string_round_trips() {
    let src = "
fn main() {
    let enc = ProtoBuf.encode_string(\"\");
    println(enc.len());
    let mut r = ProtoReader.new(enc);
    println(r.read_string());
}";
    assert_eq!(run(src), vec!["1\n", "\n"]);
}

#[test]
fn len_delim_bytes_round_trip() {
    let src = "
fn main() {
    let data = Array[1u8, 2u8, 3u8, 250u8];
    let mut payload = Vec.new();
    payload.extend_from_slice(data);
    let mut r = ProtoReader.new(ProtoBuf.encode_len_delim(payload));
    let got = r.read_len_delim();
    println(got.len());
    println(got[0]);
    println(got[3]);
}";
    assert_eq!(run(src), vec!["4\n", "1\n", "250\n"]);
}

#[test]
fn fixed64_round_trips() {
    let src = "
fn main() {
    let v = 1234567890123u64;
    let enc = ProtoBuf.encode_fixed64(v);
    println(enc.len());
    let mut r = ProtoReader.new(enc);
    println(r.read_fixed64());
}";
    assert_eq!(run(src), vec!["8\n", "1234567890123\n"]);
}

#[test]
fn fixed32_round_trips() {
    let src = "
fn main() {
    let enc = ProtoBuf.encode_fixed32(3735928559u32);
    println(enc.len());
    let mut r = ProtoReader.new(enc);
    println(r.read_fixed32());
}";
    assert_eq!(run(src), vec!["4\n", "3735928559\n"]);
}

#[test]
fn multi_field_message_decodes_in_order() {
    // Encode a tiny two-field message by hand (tag+value pairs) and walk it
    // with the reader's tag-dispatch loop — the shape generated decoders use.
    let src = "
fn main() {
    let mut buf = Vec.new();
    buf.extend_from_slice(ProtoBuf.encode_tag(1, ProtoBuf.wire_varint()));
    buf.extend_from_slice(ProtoBuf.encode_varint(42u64));
    buf.extend_from_slice(ProtoBuf.encode_tag(2, ProtoBuf.wire_len_delim()));
    buf.extend_from_slice(ProtoBuf.encode_string(\"kara\"));

    let mut r = ProtoReader.new(buf);
    while r.at_end() == false {
        let (field, wt) = r.read_tag();
        if field == 1 {
            println(r.read_varint());
        } else if field == 2 {
            println(r.read_string());
        } else {
            r.skip_field(wt);
        }
    }
}";
    assert_eq!(run(src), vec!["42\n", "kara\n"]);
}

#[test]
fn skip_field_advances_past_unknown_fields() {
    // An unknown field 3 (varint) sits between known fields 1 and 2; the
    // decoder skips it and still reads field 2.
    let src = "
fn main() {
    let mut buf = Vec.new();
    buf.extend_from_slice(ProtoBuf.encode_tag(1, ProtoBuf.wire_varint()));
    buf.extend_from_slice(ProtoBuf.encode_varint(7u64));
    buf.extend_from_slice(ProtoBuf.encode_tag(3, ProtoBuf.wire_varint()));
    buf.extend_from_slice(ProtoBuf.encode_varint(999u64));
    buf.extend_from_slice(ProtoBuf.encode_tag(2, ProtoBuf.wire_varint()));
    buf.extend_from_slice(ProtoBuf.encode_varint(8u64));

    let mut r = ProtoReader.new(buf);
    while r.at_end() == false {
        let (field, wt) = r.read_tag();
        if field == 1 {
            println(r.read_varint());
        } else if field == 2 {
            println(r.read_varint());
        } else {
            r.skip_field(wt);
        }
    }
}";
    assert_eq!(run(src), vec!["7\n", "8\n"]);
}
