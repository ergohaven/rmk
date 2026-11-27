#!/usr/bin/env python3
"""Execute production module-selection runtime with mocked hardware APIs."""
from pathlib import Path
import tempfile,subprocess,shutil
s=(Path(__file__).resolve().parents[1]/'keyboards/k04/src/module_settings.rs').read_text();start=s.index('fn apply_module_selection_runtime()');end=s.index('\nfn byte(',start);method=s[start:end]
h='''#![allow(dead_code)]
use std::sync::atomic::{AtomicU16,Ordering};
static INTERVAL:[AtomicU16;2]=[AtomicU16::new(40),AtomicU16::new(40)];
mod board {pub const K04_EXTENDED_SETTINGS:bool=true;pub const PARK_UNSELECTED_ENCODERS:bool=false;pub fn set_split_link_pointing(_:usize,_:bool){}}
#[derive(Clone,Copy,PartialEq)] enum ModuleSelection {Encoder,Trackball}
impl ModuleSelection {fn is_pointing(self)->bool {self==Self::Trackball}}
fn module_selections()->[ModuleSelection;2] {[ModuleSelection::Encoder;2]}
struct Watch; static MODULE_SELECTIONS:Watch=Watch;
impl Watch {fn sender(&self)->&Self{self} fn send_if_modified(&self,f:impl FnOnce(&mut Option<[ModuleSelection;2]>)->bool){f(&mut None);}}
mod rmk {pub mod input_device {pub mod rotary_encoder {pub fn set_encoder_enabled(_:u8,_:bool){} pub fn set_encoder_interval_ms(id:u8,value:u16)->bool{crate::INTERVAL[id as usize].store(value,crate::Ordering::Relaxed);true}}}}
'''+method+'''
#[test] fn k04_encoder_runtime_interval_is_zero_after_saved_or_static_delay() {
 for saved in [0,5,10,15,20,30,40,60,80,100] {for side in 0..2 {INTERVAL[side].store(saved,Ordering::Relaxed);}apply_module_selection_runtime();for side in 0..2 {assert_eq!(INTERVAL[side].load(Ordering::Relaxed),0);}}
}
'''
with tempfile.TemporaryDirectory(prefix='k04-encoder-') as d:
 p=Path(d)/'test.rs';p.write_text(h);exe=Path(d)/'test';subprocess.run([shutil.which('rustc') or 'rustc','--edition=2021','--test',str(p),'-o',str(exe)],check=True);subprocess.run([str(exe)],check=True)
