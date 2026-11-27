#!/usr/bin/env python3
"""Verbatim production timers/settings/motion, mocked clock/keymap/report sink.
Not a firmware scheduler, split-radio or hardware acceptance test.
"""
from pathlib import Path
import subprocess, tempfile, shutil
ROOT = Path(__file__).resolve().parents[1]
s = (ROOT / 'rmk/src/input_device/pointing.rs').read_text()
def item(start):
    pos=s.index(start); opening=s.index('{',pos); depth=0
    for i in range(opening,len(s)):
        depth+=(s[i]=='{')-(s[i]=='}')
        if not depth: return s[pos:i+1]
    raise AssertionError(start)
runtime=s[s.index('const QUBE_USER_SNIPER'):s.index('#[derive(Clone, Copy)]\nstruct QubePointingSideState')]
arming=s[s.index('#[derive(Clone, Copy)]\nstruct AutoLayerArming'):s.index('/// K:04 Qube central pointing processor.')]
methods='\n'.join(item(n) for n in ['    async fn poll(', '    fn sticky_keeps_auto_layer_access(', '    fn sync_auto_layer_for_motion(', '    fn deactivate_auto_layer('])
h='''#![allow(dead_code)]
use std::cell::Cell;
use std::sync::atomic::{AtomicU32,Ordering};
static NOW:AtomicU32=AtomicU32::new(0);
fn now_ms_u32()->u32 {NOW.load(Ordering::Relaxed)}
fn clock(t:u32){NOW.store(t,Ordering::Relaxed);}
async fn send_mouse_report_unchecked(_:u8,_:i16,_:i16,_:i16,_:i16){}
fn block_on(f:impl std::future::Future<Output=()>) {
 let mut f=std::pin::pin!(f);let mut cx=std::task::Context::from_waker(std::task::Waker::noop());
 assert!(matches!(f.as_mut().poll(&mut cx),std::task::Poll::Ready(())));
}
struct ModeKey {latched:Option<QubePointingMode>}
struct Side {touch_buttons:u8,touch_last_report_ms:u32,mode_key:ModeKey}
struct Map {active:Cell<bool>}
impl Map {
 fn mouse_buttons(&self)->u8 {0}
 fn is_layer_active(&self,_:u8)->bool {self.active.get()}
 fn activate_layer_if_inactive(&self,_:u8)->bool {!self.active.replace(true)}
 fn deactivate_layer_if_active(&self,_:u8){self.active.set(false);}
}
struct Processor {settings:QubePointingSettings,sides:[Side;2],keymap:Map,
 active_auto_layer:u8,auto_layer_held_keys:Vec<u8>,auto_layer_self_activated:bool,
 last_auto_motion_ms:u32,auto_layer_arming:AutoLayerArming}
impl Processor {
 fn new()->Self {Self {settings:QubePointingSettings::new(),sides:std::array::from_fn(|_|Side {touch_buttons:0,touch_last_report_ms:0,mode_key:ModeKey {latched:None}}),keymap:Map {active:Cell::new(false)},active_auto_layer:QUBE_AUTO_LAYER_NONE,auto_layer_held_keys:Vec::new(),auto_layer_self_activated:false,last_auto_motion_ms:0,auto_layer_arming:AutoLayerArming::new()}}
 fn sync_touch_buttons(&self){}
'''+methods+'}\n'+runtime+arming+'''
#[test] fn actual_poll_expiry_renewal_held_and_legacy_controls() {
 for extended in [false,true] {
 let max=if extended {if cfg!(feature="k04_standalone_timeout_14"){19}else{11}}else{QUBE_AUTO_LAYER_TIMEOUT_MAX_INDEX};
 for idx in 0..=max {
  let mut p=Processor::new();p.settings.k04_extended=extended;p.settings.auto_layer_timeout_index=idx;
  let t=p.settings.auto_layer_timeout_ms();
  if extended && idx==max {assert_eq!(t,3000);}
  if !extended && idx==max {assert_eq!(t,1500);}
  clock(10);p.sync_auto_layer_for_motion(QubePointingMode::Normal,QubePointingSource{side:0,kind:QubePointingKind::Ball},1,0);
  assert!(p.keymap.active.get());
  clock(10+t-1);block_on(p.poll());assert!(p.keymap.active.get());
  p.sync_auto_layer_for_motion(QubePointingMode::Normal,QubePointingSource{side:0,kind:QubePointingKind::Ball},1,0);
  clock(10+t);block_on(p.poll());assert!(p.keymap.active.get(),"motion renewed timer");
  clock(10+t-1+t);p.auto_layer_held_keys.push(1);block_on(p.poll());assert!(p.keymap.active.get(),"held key guards expiry");
  p.auto_layer_held_keys.clear();block_on(p.poll());assert!(!p.keymap.active.get(),"expires at exact timeout");
  // Wrapping u32 uptime retains the same exact comparison.
  clock(u32::MAX-20);p.sync_auto_layer_for_motion(QubePointingMode::Normal,QubePointingSource{side:0,kind:QubePointingKind::Ball},1,0);
  clock((u32::MAX-20).wrapping_add(t));block_on(p.poll());assert!(!p.keymap.active.get());
 }}
}
#[test] fn actual_motion_threshold_only_ball_and_touch_hold_timeout() {
 let mut p=Processor::new();p.settings.k04_extended=true;
 p.settings.auto_layer_timeout_index=if cfg!(feature="k04_standalone_timeout_14"){19}else{11};
 p.settings.auto_layer_threshold=100;
 clock(100);p.sync_auto_layer_for_motion(QubePointingMode::Normal,QubePointingSource{side:0,kind:QubePointingKind::Ball},254,0);assert!(!p.keymap.active.get());
 clock(3201);p.sync_auto_layer_for_motion(QubePointingMode::Normal,QubePointingSource{side:0,kind:QubePointingKind::Ball},1,0);assert!(!p.keymap.active.get(),"separate nudges expire");
 p.sync_auto_layer_for_motion(QubePointingMode::Normal,QubePointingSource{side:1,kind:QubePointingKind::Touch},1,0);assert!(p.keymap.active.get(),"touch bypasses ball threshold");
 p.sides[1].touch_buttons=1;p.sides[1].touch_last_report_ms=6201;
 clock(6201);block_on(p.poll());assert!(p.keymap.active.get(),"touch hold guards expiry");
 clock(6701);block_on(p.poll());assert_eq!(p.sides[1].touch_buttons,0);assert!(!p.keymap.active.get());
}
'''
with tempfile.TemporaryDirectory(prefix='k04-timers-') as d:
    p=Path(d)/'test.rs'; p.write_text(h); exe=Path(d)/'test'
    for args in [[],['--cfg','feature="k04_standalone_timeout_14"']]:
        subprocess.run([shutil.which('rustc') or 'rustc','--edition=2021','--test',str(p),'-o',str(exe),*args],check=True)
        subprocess.run([str(exe),'--test-threads=1'],check=True)
