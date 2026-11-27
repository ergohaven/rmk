#!/usr/bin/env python3
"""Focused actual-source codec/descriptor regression. Run under agent-storage."""
from pathlib import Path
import re, json, subprocess, tempfile, shutil
ROOT=Path(__file__).resolve().parents[1]
s=(ROOT/'keyboards/k04/src/layer_names.rs').read_text()
def fn(name):
 m=re.search(r'^(?:pub(?:\(crate\))? )?(?:const )?fn '+name+r'\b',s,re.M); assert m,name
 start=s.index('{',m.start()); depth=0
 for i in range(start,len(s)):
  depth+=(s[i]=='{')-(s[i]=='}')
  if not depth:return s[m.start():i+1]
 raise AssertionError(name)
constants=s[s.index('const MODULE_SETTINGS_VERSION'):s.index('const _: () = assert!(MODULE_DEFAULTS[IDX_FLAGS]')]
board='\n'.join(l for l in (ROOT/'keyboards/k04/src/board.rs').read_text().splitlines() if l.startswith(('pub const DEFAULT_', 'pub const AUTO_MODULES','pub const K04_')))
functions=['serialize_module_settings','deserialize_module_settings','module_settings_sync_packet','module_encoder_settings_sync_packet','ensure_module_settings_initialized','reset_module_settings','module_byte','module_set_byte','module_layer_color_index','module_set_layer_color_index','module_bt_profile_color_index','module_set_bt_profile_color_index','pack_color','unpack_color','set_default_layer_color','module_get_setting','module_qsid_width','module_qsid_value','module_set_setting','module_flag','module_set_flag','module_axis_flag','module_set_axis_flag','module_auto_flag','module_set_auto_flag','module_touch_gestures','module_set_touch_gestures','module_selection_value','module_set_module_selection']
pointing=(ROOT/'rmk/src/input_device/pointing.rs').read_text()
runtime=pointing[pointing.index('const QUBE_USER_SNIPER'):pointing.index('#[derive(Clone, Copy)]\nstruct QubePointingSideState')]
tests='''
#[test] fn query_omits_intervals_but_legacy_get_set_preserves_steps() {
 assert!(!SETTING_KEYS.contains(&325) && !SETTING_KEYS.contains(&326));
 assert!(SETTING_KEYS.contains(&332) && SETTING_KEYS.contains(&333));
 for side in 0..2 { for step in 0..=7 { for interval in 0..=9 {
  assert!(module_set_setting(332+side,&[step]));
  assert!(module_set_setting(325+side,&[interval]));
  let mut got=[255];assert_eq!(module_get_setting(325+side,&mut got),Some(1));assert_eq!(got,[interval]);
  assert_eq!(module_get_setting(332+side,&mut got),Some(1));assert_eq!(got,[step]);
  let stored=serialize_module_settings();deserialize_module_settings(&stored);
  assert_eq!(module_get_setting(325+side,&mut got),Some(1));assert_eq!(got,[interval]);
  assert_eq!(module_get_setting(332+side,&mut got),Some(1));assert_eq!(got,[step]);
 }}}
}
#[test] fn extended_timeout_storage_sync_all_indices_preserve_encoder_bits() {
 let max=if cfg!(feature="standalone") {19}else{11};
 assert_eq!(AUTO_LAYER_TIMEOUT_MAX_INDEX,max);
 for idx in 0..=max { for interval in 0..=9 {
 reset_module_settings(); module_set_byte(IDX_AUTO_LAYER_TIMEOUT,idx); module_set_byte(IDX_RIGHT_ENCODER_INTERVAL,interval);
 let stored=serialize_module_settings(); assert_eq!(stored.len(),34); assert_eq!(stored[30]>>4,interval);
 deserialize_module_settings(&stored); assert_eq!(module_byte(IDX_AUTO_LAYER_TIMEOUT),idx); assert_eq!(module_byte(IDX_RIGHT_ENCODER_INTERVAL),interval);
 let pkt=module_settings_sync_packet(); assert_eq!(pkt.len(),27);
 assert_eq!((pkt[26]>>4)|((pkt[25]&0x40)>>2),idx); assert_ne!(pkt[25]&0x80,0);
 let mut runtime=QubePointingSettings::new();runtime.apply_packet(&pkt);
 let ms=if cfg!(feature="standalone") {[50,100,150,200,250,300,350,400,450,500,750,1000,1250,1500,1750,2000,2250,2500,2750,3000][idx as usize]}else{u32::from(idx+1)*250};
 assert_eq!(runtime.auto_layer_timeout_ms(),ms);
 let mut legacy=pkt;legacy[25]&=0x3f;runtime.apply_packet(&legacy);
 assert_eq!(runtime.auto_layer_timeout_ms(),QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE[usize::from((pkt[26]>>4).min(QUBE_AUTO_LAYER_TIMEOUT_MAX_INDEX))]);
 }}
}
#[test] fn legacy_defaults_indices_and_v9_migration_preserved() {
 reset_module_settings(); assert_eq!(module_byte(IDX_AUTO_LAYER_TIMEOUT),if cfg!(feature="standalone"){9}else{1});
 for idx in 0..6 {let mut data=[0;34];data[0]=9;data[30]=(9<<4)|idx;deserialize_module_settings(&data);
 assert_eq!(module_byte(IDX_AUTO_LAYER_TIMEOUT),if cfg!(feature="standalone"){[4,9,10,11,12,13][idx as usize]}else{idx});assert_eq!(module_byte(IDX_RIGHT_ENCODER_INTERVAL),9);}
}
'''
keys=s[s.index('const BASE_SETTING_KEYS'):s.index('static DISPLAY_SETTINGS')]
h='\n'.join(['#![allow(dead_code)]','use std::sync::atomic::{AtomicU8,Ordering};',constants,runtime,keys,'static MODULE_SETTINGS:[AtomicU8;MODULE_SETTINGS_LEN]=[const {AtomicU8::new(0)};MODULE_SETTINGS_LEN];',f'mod board {{{board}}}','fn publish_module_settings() {} fn set_display_setting(_:u16,_:u8) {} static DISPLAY_SETTINGS:[AtomicU8;3]=[const {AtomicU8::new(0)};3];',*(fn(n) for n in functions),tests])
with tempfile.TemporaryDirectory(prefix='k04-followon-') as d:
 p=Path(d)/'test.rs';p.write_text(h)
 for profile,args in [('standalone',['--cfg','feature="standalone"','--cfg','feature="k04_standalone_timeout_14"']),('qube',[])]:
  exe=Path(d)/'test';subprocess.run([shutil.which('rustc'),'--edition=2021','--test',str(p),'-o',str(exe),*args],check=True);subprocess.run([str(exe),'--test-threads=1'],check=True)
for p in sorted((ROOT/'keyboards/k04').glob('vial*.json')):
 j=json.loads(p.read_text());fields=[f for g in j['settings'] for f in g['fields']]
 assert not any(f['qsid'] in [325,326] for f in fields),p
 timeout=next(f for f in fields if f['qsid']==324)
 old=[250,500,750,1000,1250,1500] if 'qube' in p.name else [50,100,150,200,250,300,350,400,450,500,750,1000,1250,1500]
 assert timeout['variants']==[f'{v} ms' for v in old+[1750,2000,2250,2500,2750,3000]],p
 assert [(k['name'],k['shortName']) for k in j['customKeycodes'][-3:]]==[('EH_L_NORMAL','Left\nNormal'),('EH_NORMAL','Normal\nmode'),('EH_R_NORMAL','Right\nNormal')],p
print('PASS 12 descriptors; appended legacy-compatible timeout indices, no encoder interval, 3 Normal actions')
