#![cfg(feature = "split")]

#[path = "../../keyboards/phenom/src/role.rs"]
mod role;

use core::future::{pending, ready};

use embassy_futures::block_on;
use rmk::event::{KeyboardEvent, KeyboardEventSource};

#[test]
fn startup_elects_only_an_enumerated_usb_half() {
    assert!(block_on(role::primary(ready(()), pending())));
    assert!(!block_on(role::primary(pending(), ready(()))));
    assert!(
        block_on(role::primary(ready(()), ready(()))),
        "enumeration wins at the deadline"
    );
}

#[test]
fn either_primary_preserves_every_physical_key_position() {
    for (rows, cols) in [(10, 8)] {
        for right in [false, true] {
            let local = role::matrix_config(right, rows, cols);
            for row in 0..local.rows {
                for col in 0..local.cols {
                    for pressed in [false, true] {
                        let global = KeyboardEvent::key(row + local.row_offset, col, pressed);
                        let wire = role::local_matrix_event(global, local).unwrap();
                        assert_eq!(wire, KeyboardEvent::key(row, col, pressed));
                        let KeyboardEventSource::Key(pos) = wire.source else {
                            unreachable!()
                        };
                        let received = KeyboardEvent::key(pos.row + local.row_offset, pos.col, wire.pressed);
                        assert_eq!(received, global);
                    }
                }
            }
            assert!(role::local_matrix_event(KeyboardEvent::key(rows as u8, 0, true), local).is_none());
            assert!(role::local_matrix_event(KeyboardEvent::key(local.row_offset, cols as u8, true), local).is_none());
            if right {
                assert!(role::local_matrix_event(KeyboardEvent::key(0, 0, true), local).is_none());
            }
        }
    }
}

#[test]
fn module_identity_does_not_depend_on_usb_role() {
    assert_eq!(role::module_ids(false), (0, 2));
    assert_eq!(role::module_ids(true), (1, 3));
}
