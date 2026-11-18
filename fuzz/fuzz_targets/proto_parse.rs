//! Every network parser must survive arbitrary bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;
use mirrorwire_proto::{control::Control, cursor, fragment::PacketHeader, nack::Nack};

fuzz_target!(|data: &[u8]| {
    let _ = PacketHeader::parse(data);
    let _ = Nack::decode(data);
    let _ = cursor::PointerUpdate::decode(data);
    let _ = cursor::CursorImage::decode(data);
    let _ = cursor::is_pointer_datagram(data);
    let _ = Control::decode_body(data);
});
