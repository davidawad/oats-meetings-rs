//! A shared handle on one local-socket stream, usable as an independent reader and writer
//! (`&Stream` implements `Read` and `Write`; interprocess deprecated `split` in favour of this).

use std::io::{Read, Write};
use std::sync::Arc;

use interprocess::local_socket::Stream;

#[derive(Clone)]
pub struct Half(pub Arc<Stream>);

impl Half {
    pub fn pair(stream: Stream) -> (Half, Half) {
        let a = Arc::new(stream);
        (Half(Arc::clone(&a)), Half(a))
    }
}

impl Read for Half {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for Half {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}
