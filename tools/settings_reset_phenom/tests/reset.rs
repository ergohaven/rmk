#[path = "../src/reset.rs"]
mod reset;
use reset::*;
use std::cell::RefCell;

#[test]
fn exact_current_phenom_partition() {
    assert_eq!((START, END, PAGE_SIZE, FLASH_SIZE), (0x100000, 0x120000, 4096, 0x200000));
    let calls = RefCell::new(Vec::new());
    assert!(reset_settings(|a,b| { calls.borrow_mut().push((a,b)); true }));
    assert_eq!(*calls.borrow(), (0..32).map(|n| (0x100000+n*4096,0x101000+n*4096)).collect::<Vec<_>>());
}
#[test]
fn bounded_retries_and_fail_closed() {
    let calls = RefCell::new(Vec::new());
    assert!(!reset_settings(|a,b| { calls.borrow_mut().push((a,b)); false }));
    assert_eq!(*calls.borrow(), vec![(START,START+PAGE_SIZE);3]);
    let mut attempts=0;
    assert!(reset_settings(|_,_| { attempts+=1; attempts%3==0 }));
    assert_eq!(attempts,96);
}
#[test]
fn rejects_underflow_unaligned_overlap_and_out_of_bounds() {
    assert!(valid_range(START,END,FLASH_SIZE,START));
    for (s,e,c,code) in [(END,START,FLASH_SIZE,START),(START,START,FLASH_SIZE,START),(START+1,END,FLASH_SIZE,START),(START,END+1,FLASH_SIZE,START),(START,END,END-1,START),(START,END,FLASH_SIZE,START+1),(0,END,FLASH_SIZE,256),(0xfffff000,0xffff0000,FLASH_SIZE,256)] {
        assert!(!valid_range(s,e,c,code), "{s:x} {e:x} {c:x} {code:x}");
    }
}
