#!/usr/bin/env python3
"""Compile verbatim mode state/handler with simulated processor surroundings."""
from pathlib import Path
import re,tempfile,subprocess,shutil
root=Path(__file__).resolve().parents[1];s=(root/'rmk/src/input_device/pointing.rs').read_text()
def item(start):
 pos=s.index(start);opening=s.index('{',pos);depth=0
 for i in range(opening,len(s)):
  depth+=(s[i]=='{')-(s[i]=='}')
  if not depth:return s[pos:i+1]
 raise AssertionError(start)
state=s[s.index('#[derive(Clone, Copy)]\npub struct PointingModeKeyState'):s.index('/// Configuration for cursor mode')]
methods=[item('    fn mode_for_side('),item('    fn handle_mode_key(')]
h='''#![allow(dead_code)]
#[derive(Clone,Copy,Debug,PartialEq,Eq)] enum QubePointingMode {Normal,Sniper,Scroll,Text}
#[derive(Clone,Copy,PartialEq,Eq)] enum QubeModeKeyScope {Both,Side}
struct Side {mode_key:PointingModeKeyState<QubePointingMode>,mode_key_scope:QubeModeKeyScope,text_last_motion_ms:u32}
impl Side {fn reset_accum(&mut self){}}
struct Settings {mode:[QubePointingMode;2],sticky:[bool;2]}
impl Settings {fn sticky_mode(&self,side:usize)->bool {self.sticky[side]}}
struct Processor {settings:Settings,sides:[Side;2]}
impl Processor {fn reevaluate_auto_layer(&mut self){}
'''+ '\n'.join(methods)+'}\n'+state+'''
#[test] fn explicit_normal_overrides_saved_base_and_stale_releases() {
 for base in [QubePointingMode::Normal,QubePointingMode::Scroll,QubePointingMode::Text] {for sticky in [false,true] {for scope in [[true,false],[true,true],[false,true]] {
 let mut p=Processor {settings:Settings {mode:[base;2],sticky:[sticky;2]},sides:std::array::from_fn(|_|Side {mode_key:PointingModeKeyState::new(),mode_key_scope:QubeModeKeyScope::Both,text_last_motion_ms:0})};
 p.handle_mode_key([true,true],QubePointingMode::Sniper,true);
 p.handle_mode_key(scope,QubePointingMode::Normal,true);p.handle_mode_key(scope,QubePointingMode::Normal,false);
 p.handle_mode_key([true,true],QubePointingMode::Sniper,false);
 for side in 0..2 {if scope[side] {assert_eq!(p.mode_for_side(side),QubePointingMode::Normal,"base={base:?} sticky={sticky} scope={scope:?}");}}
 assert_eq!(p.settings.mode,[base;2]);assert_eq!(p.settings.sticky,[sticky;2]);
 }}}
}
'''
with tempfile.TemporaryDirectory(prefix='k04-normal-') as d:
 p=Path(d)/'test.rs';p.write_text(h);exe=Path(d)/'test';subprocess.run([shutil.which('rustc') or 'rustc','--edition=2021','--test',str(p),'-o',str(exe)],check=True);subprocess.run([str(exe)],check=True)
