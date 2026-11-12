#!/usr/bin/env python3
"""Host probe of verbatim production BLE bootstrap and confirmation code.

Simulated boundaries: controller submission, Connection::params, Signal/event
arrival and time. Not a radio/macOS/Windows/HID transport test. No rewritten policy.
"""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'rmk/src/ble/mod.rs').read_text()

def function(name):
    match = re.search(r'^(?:(?:pub\(crate\) )?async |const )?fn ' + name + r'\b', source, re.M)
    assert match, name
    begin = source.index('{', match.start())
    depth = 0
    for pos in range(begin, len(source)):
        if source[pos] == '{': depth += 1
        if source[pos] == '}':
            depth -= 1
            if not depth: return source[match.start():pos + 1]
    raise AssertionError(name)

# Keep the complete real bootstrap expression, including its return policy.
start = source.index('    #[cfg(feature = "host_fixed_15ms")]\n    let mut active_params = {')
end = source.index('\n    if let Some(config) = host_power_config {', start)
bootstrap = source[start:end]
prelude = r'''
#![allow(dead_code, unused_mut, unused_variables)]
use std::{cell::RefCell, marker::PhantomData, future::Future, pin::pin,
    task::{Context, Poll, Waker}};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Duration(u64);
impl Duration {
 const fn from_micros(n:u64)->Self{Self(n)}
 const fn from_millis(n:u64)->Self{Self(n*1000)}
 const fn from_secs(n:u64)->Self{Self(n*1000000)}
 fn as_micros(self)->u64{self.0}
 fn as_millis(self)->u64{self.0/1000}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostConnParamsSnapshot {interval: Duration, latency:u16}
#[derive(Clone, Copy)]
struct ConnParams {conn_interval:Duration, peripheral_latency:u16}
#[derive(Clone, Copy)]
struct RequestedConnParams {min_connection_interval:Duration,max_connection_interval:Duration,
 max_latency:u16,min_event_length:Duration,max_event_length:Duration,supervision_timeout:Duration}
#[derive(Clone, Copy)]
struct Scenario {current:HostConnParamsSnapshot, after:Option<HostConnParamsSnapshot>,
 event:Option<HostConnParamsSnapshot>, submitted:bool, updates:u32}
thread_local!{static S: RefCell<Scenario> = RefCell::new(Scenario {
 current: snap(30,0), after:None, event:None, submitted:true, updates:0});}
fn snap(ms:u64, latency:u16)->HostConnParamsSnapshot {
 HostConnParamsSnapshot{interval:Duration::from_millis(ms),latency}
}
trait Controller {}
trait ControllerCmdSync<T>{}
struct LeReadLocalSupportedFeatures;
trait PacketPool{}
struct C; impl Controller for C{} impl ControllerCmdSync<LeReadLocalSupportedFeatures> for C{}
struct P; impl PacketPool for P{}
struct Stack<'a,C,P>(PhantomData<(&'a (),C,P)>);
struct Connection<'a,P>(PhantomData<(&'a (),P)>);
impl<P> Connection<'_,P>{
 fn params(&self)->ConnParams { S.with(|s|{let p=s.borrow().current;
 ConnParams{conn_interval:p.interval,peripheral_latency:p.latency}}) }
 fn is_connected(&self)->bool { true }
 fn raw(&self)->&Self {self}
}
struct Signal; static HOST_CONN_PARAMS_UPDATED:Signal=Signal;
impl Signal {
 fn reset(&self){}
 async fn wait(&self)->HostConnParamsSnapshot {
   if let Some(p)=S.with(|s|s.borrow().event){p}else{std::future::pending().await}
 }
}
struct Timer;
impl Timer {async fn after_secs(_:u64){} async fn after_millis(_:u64){}}
async fn with_timeout<F:Future>(_:Duration,f:F)->Result<F::Output,()> {
 let mut f=pin!(f);let mut cx=Context::from_waker(Waker::noop());
 match f.as_mut().poll(&mut cx){Poll::Ready(v)=>Ok(v),Poll::Pending=>Err(())}
}
async fn update_conn_params<C,P>(_:&Stack<'_,C,P>,_:&Connection<'_,P>,_:&RequestedConnParams)->bool{
 S.with(|s|{let mut s=s.borrow_mut();s.updates+=1;if let Some(p)=s.after{s.current=p;}s.submitted})
}
macro_rules! info {($($t:tt)*)=>{{}}}
macro_rules! warn {($($t:tt)*)=>{{}}}
macro_rules! error {($($t:tt)*)=>{{}}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum BleKeyboardExit {ConnectionParamsStalled, Running(HostConnParamsSnapshot)}
fn block_on<F:Future>(f:F)->F::Output {
 let mut f=pin!(f);let mut cx=Context::from_waker(Waker::noop());
 match f.as_mut().poll(&mut cx){Poll::Ready(v)=>v,Poll::Pending=>panic!("unhandled boundary")}
}
'''
constants = '\n'.join(re.search(r'^const ' + n + r':[^\n]+', source, re.M).group(0) for n in [
    'HOST_INTERACTIVE_MAX_LATENCY', 'HOST_CONN_PARAM_UPDATE_TIMEOUT_SECS',
    'HOST_ACTIVE_CONN_PARAM_ATTEMPTS', 'HOST_ACTIVE_CONN_PARAM_RETRY_MS',
    'HOST_FIXED_CONN_PARAM_ATTEMPTS', 'HOST_FIXED_CONN_PARAM_RETRY_MS'])
functions = constants + '\n' + '\n'.join(function(n) for n in [
    'host_connection_params', 'host_bootstrap_connection_requests',
    'host_requires_apple_safe_fallback', 'host_interactive_target',
    'host_active_params_confirmed', 'request_confirmed_active_params'])
wrapper = '''\nasync fn bootstrap(stack:&Stack<'_,C,P>,conn:&Connection<'_,P>,host_power_config:Option<()>) -> BleKeyboardExit {\n''' + bootstrap + '''\n BleKeyboardExit::Running(active_params)\n}\n'''
tests = r'''
fn setup(current:HostConnParamsSnapshot, after:Option<HostConnParamsSnapshot>,
         event:Option<HostConnParamsSnapshot>, submitted:bool) {
 S.with(|s|*s.borrow_mut()=Scenario{current,after,event,submitted,updates:0});
}
fn boot()->BleKeyboardExit {
 block_on(bootstrap(&Stack(PhantomData),&Connection(PhantomData),Some(())))
}
fn confirm(target:HostConnParamsSnapshot)->Option<HostConnParamsSnapshot> {
 block_on(request_confirmed_active_params(&Stack::<C,P>(PhantomData),
 &Connection::<P>(PhantomData),target,1,HOST_ACTIVE_CONN_PARAM_ATTEMPTS,HOST_ACTIVE_CONN_PARAM_RETRY_MS))
}
#[test] fn startup_keeps_host_selected_30ms_link() {
 setup(snap(30,0),None,Some(snap(30,0)),true);
 assert_eq!(boot(),BleKeyboardExit::Running(snap(30,0)));
}
#[test] fn startup_missing_event_does_not_disconnect_usable_link() {
 setup(snap(30,0),None,None,true);
 assert_eq!(boot(),BleKeyboardExit::Running(snap(30,0)));
}
#[test] fn macos_11250us_without_update_event_keeps_connection() {
 let accepted=HostConnParamsSnapshot{interval:Duration::from_micros(11250),latency:0};
 setup(accepted,None,None,true);
 assert_eq!(boot(),BleKeyboardExit::Running(accepted));
 assert_eq!(S.with(|s|s.borrow().updates),3);
}
#[test] fn macos_current_parameters_override_stale_mismatching_event() {
 let accepted=HostConnParamsSnapshot{interval:Duration::from_micros(11250),latency:0};
 setup(accepted,None,Some(snap(30,30)),true);
 assert_eq!(confirm(snap(15,0)),Some(accepted));
}
#[test] fn startup_rejection_preserves_link_but_wake_target_has_zero_latency() {
 setup(snap(30,30),None,None,false);
 assert_eq!(boot(),BleKeyboardExit::Running(snap(30,0)));
}
#[test] fn wake_reads_actual_state_after_missing_event() {
 setup(snap(30,30),Some(snap(30,0)),None,true);
 assert_eq!(confirm(snap(30,0)),Some(snap(30,0)));
}
#[test] fn stale_matching_event_cannot_confirm_current_idle_state() {
 setup(snap(30,30),None,Some(snap(30,0)),true);
 assert_eq!(confirm(snap(30,0)),None);
}
#[test] fn wake_still_fails_when_latency_is_not_restored() {
 setup(snap(30,30),None,None,true);
 assert_eq!(confirm(snap(30,0)),None);
 assert_eq!(S.with(|s|s.borrow().updates),2);
}
#[test] fn wake_still_rejects_slower_than_accepted_anchor() {
 setup(snap(45,0),None,Some(snap(45,0)),true);
 assert_eq!(confirm(snap(30,0)),None);
}
#[test] fn preferred_startup_still_uses_fast_zero_latency_parameters() {
 let fast=HostConnParamsSnapshot{interval:Duration::from_micros(7500),latency:0};
 setup(fast,None,Some(fast),true);
 assert_eq!(boot(),BleKeyboardExit::Running(fast));
}
'''
with tempfile.TemporaryDirectory(prefix='k04-host-params-') as directory:
    path = Path(directory)
    (path/'probe.rs').write_text(prelude + functions + wrapper + tests)
    subprocess.run(['rustc','--edition=2021','--test',str(path/'probe.rs'),'-o',str(path/'probe')],check=True)
    subprocess.run([str(path/'probe'),'--nocapture'],check=True)
