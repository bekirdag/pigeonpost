//! A tiny, strict, length-prefixed codec for this crate's own structures (credential identity,
//! AAD, payloads, persisted state). MLS objects themselves always use OpenMLS's TLS codec.
//!
//! Every reader is bounds-checked, rejects trailing bytes via [`Reader::finish`], and never
//! allocates more than the input length, so hostile input cannot trigger large allocations.

use crate::error::{Error, Result};

#[derive(Default)]
pub(crate) struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(bytes);
        self
    }

    pub(crate) fn u8(&mut self, value: u8) -> &mut Self {
        self.buf.push(value);
        self
    }

    pub(crate) fn u16(&mut self, value: u16) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(crate) fn u32(&mut self, value: u32) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(crate) fn u64(&mut self, value: u64) -> &mut Self {
        self.buf.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// A field of at most 65535 bytes. Callers validate lengths first; this is a hard stop.
    pub(crate) fn bytes16(&mut self, bytes: &[u8]) -> Result<&mut Self> {
        let len = u16::try_from(bytes.len()).map_err(|_| Error::invalid("field too long"))?;
        self.u16(len);
        Ok(self.raw(bytes))
    }

    pub(crate) fn bytes32(&mut self, bytes: &[u8]) -> Result<&mut Self> {
        let len = u32::try_from(bytes.len()).map_err(|_| Error::invalid("field too long"))?;
        self.u32(len);
        Ok(self.raw(bytes))
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.buf
    }
}

pub(crate) struct Reader<'a> {
    input: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { input }
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.input.len() < len {
            return Err(Error::Decode("truncated input"));
        }
        let (head, tail) = self.input.split_at(len);
        self.input = tail;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(crate) fn bytes16(&mut self) -> Result<&'a [u8]> {
        let len = usize::from(self.u16()?);
        self.take(len)
    }

    pub(crate) fn bytes32(&mut self) -> Result<&'a [u8]> {
        let len = usize::try_from(self.u32()?).map_err(|_| Error::Decode("length overflow"))?;
        self.take(len)
    }

    pub(crate) fn str16(&mut self) -> Result<&'a str> {
        core::str::from_utf8(self.bytes16()?).map_err(|_| Error::Decode("invalid UTF-8"))
    }

    pub(crate) fn finish(self) -> Result<()> {
        if self.input.is_empty() {
            Ok(())
        } else {
            Err(Error::Decode("trailing bytes"))
        }
    }
}
