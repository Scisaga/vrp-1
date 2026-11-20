use prost::Message;
use std::io::{self, Read, Write};

pub(crate) fn read_frame<M: Message + Default>(
    reader: &mut impl Read,
    limit: u32,
) -> io::Result<Option<M>> {
    let mut prefix = [0u8; 4];
    // Distinguish a clean EOF from a truncated prefix.
    loop {
        match reader.read(&mut prefix[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    reader.read_exact(&mut prefix[1..])?;
    let len = u32::from_le_bytes(prefix);
    if len == 0 || len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame exceeds limit or is empty",
        ));
    }
    let mut bytes = vec![0; len as usize];
    reader.read_exact(&mut bytes)?;
    M::decode(bytes.as_slice())
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub(crate) fn write_frame<M: Message>(
    writer: &mut impl Write,
    message: &M,
    limit: u32,
) -> io::Result<()> {
    let len = message.encoded_len();
    if len == 0 || len > limit as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame exceeds limit or is empty",
        ));
    }
    let bytes = message.encode_to_vec();
    writer.write_all(&(len as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vrp_proto::worker::v1::{ChildCommand, child_command};
    #[test]
    fn framing_rejects_oversized_truncated_and_invalid_messages() {
        assert!(read_frame::<ChildCommand>(&mut &[100, 0, 0, 0][..], 8).is_err());
        assert!(read_frame::<ChildCommand>(&mut &[2, 0][..], 8).is_err());
        assert!(read_frame::<ChildCommand>(&mut &[2, 0, 0, 0, 255][..], 8).is_err());
        assert!(
            read_frame::<ChildCommand>(&mut &[][..], 8)
                .unwrap()
                .is_none()
        );
        let command = ChildCommand {
            command: Some(child_command::Command::Cancel(true)),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &command, 8).unwrap();
        assert_eq!(
            read_frame::<ChildCommand>(&mut bytes.as_slice(), 8)
                .unwrap()
                .unwrap(),
            command
        );
    }
}
