#![cfg(loom)]

use {
    loom::{sync::Arc, thread},
    rtobj::reader::realtime_reader,
};

#[test]
fn read_and_writing_simultaneously() {
    #[derive(Clone)]
    struct Big {
        count: i64,
        _padding: [u8; 64],
    }

    impl Default for Big {
        fn default() -> Self {
            Self {
                count: 0,
                _padding: [0; 64],
            }
        }
    }

    loom::model(|| {
        let (writer, reader) = realtime_reader(Big::default());
        let writer = Arc::new(writer);

        const WRITERS: i64 = 2;
        const WRITES: i64 = 3;

        for _ in 0..WRITERS {
            thread::spawn({
                let writer = Arc::clone(&writer);
                move || {
                    for _ in 0..WRITES {
                        writer.write().count += 1;
                    }
                }
            });
        }

        let value = reader.read();
        assert!(value.count >= 0 && value.count <= WRITERS * WRITES)
    });
}
