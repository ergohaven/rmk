use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use rmk_config::SplitConnection;
use rmk_config::resolved::hardware::{BoardConfig, CommunicationConfig};
use rmk_config::resolved::{Behavior, Hardware, Host};
use syn::ItemMod;

use super::override_helper::Overwritten;
use crate::codegen::chip::flash::expand_dfu_interface;
use crate::codegen::feature::{get_rmk_features, is_feature_enabled};

#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_rmk_entry(
    hardware: &Hardware,
    host: &Host,
    behavior: &Behavior,
    item_mod: &ItemMod,
    devices: Vec<TokenStream2>,
    processors: Vec<TokenStream2>,
    registered_processors: Vec<TokenStream2>,
    watchdog_task: Option<TokenStream2>,
) -> TokenStream2 {
    // If there is a function with `#[Overwritten(entry)]`, override the entry
    let entry_override = item_mod.content.as_ref().and_then(|(_, items)| {
        items.iter().find_map(|item| {
            if let syn::Item::Fn(item_fn) = item
                && let Some(Ok(Overwritten::Entry)) =
                    super::override_helper::find_overwritten(item_fn)
            {
                let content = &item_fn.block.stmts;
                return Some(quote! { #(#content)* });
            }
            None
        })
    });
    entry_override.unwrap_or(rmk_entry_select(
        hardware,
        host,
        behavior,
        devices,
        processors,
        registered_processors,
        watchdog_task,
    ))
}

pub(crate) fn rmk_entry_select(
    hardware: &Hardware,
    host: &Host,
    behavior: &Behavior,
    devices: Vec<TokenStream2>,
    processors: Vec<TokenStream2>,
    registered_processors: Vec<TokenStream2>,
    watchdog_task: Option<TokenStream2>,
) -> TokenStream2 {
    let auto_mouse_layer_enabled = !behavior.auto_mouse_layer.is_empty();
    let auto_mouse_layer_prelude = auto_mouse_layer_enabled.then(|| {
        quote! {
            let mut auto_mouse_layer = ::rmk::AutoMouseLayerRunner::new(&keymap);
        }
    });
    let devices_task = {
        let mut devs = devices.clone();
        devs.push(quote! {matrix});
        if hardware.storage.is_some() {
            devs.push(quote! {storage});
        }
        if auto_mouse_layer_enabled {
            devs.push(quote! {auto_mouse_layer});
        }
        quote! {
            ::rmk::run_all! (
                #(#devs),*
            )
        }
    };
    let processors_task = if processors.is_empty() {
        quote! {}
    } else {
        quote! {
            ::rmk::run_all! (
                #(#processors),*
            )
        }
    };

    let board = &hardware.board;
    let communication = &hardware.communication;
    // A BLE split central's transport takes one matrix region per peripheral.
    // A 125 Hz pointing source needs one connection event per 8 ms report, so
    // only a peripheral that owns a pointing device gets the pointing cadence;
    // key-only links keep the standard 15 ms profile.
    let (split_peripheral_matrices, split_link_profiles) = match board {
        BoardConfig::Split(split) if matches!(split.connection, SplitConnection::Ble) => {
            let peripheral_matrices = split.peripheral.iter().map(|p| {
                let rows = p.rows as u8;
                let cols = p.cols as u8;
                let row_offset = p.row_offset as u8;
                let col_offset = p.col_offset as u8;
                quote! {
                    ::rmk::split::PeripheralMatrixConfig {
                        rows: #rows,
                        cols: #cols,
                        row_offset: #row_offset,
                        col_offset: #col_offset,
                    }
                }
            });
            let split_link_profiles = if split.peripheral.iter().any(|p| {
                p.input_device
                    .as_ref()
                    .is_some_and(|devices| devices.has_pointing_device())
            }) {
                let profiles = split.peripheral.iter().map(|p| {
                    if p.input_device
                        .as_ref()
                        .is_some_and(|devices| devices.has_pointing_device())
                    {
                        quote! { ::rmk::split::ble::central::SplitLinkProfile::Pointing }
                    } else {
                        quote! { ::rmk::split::ble::central::SplitLinkProfile::Keyboard }
                    }
                });
                quote! { .with_split_link_profiles([#(#profiles),*]) }
            } else {
                quote! {}
            };
            (
                quote! { , [#(#peripheral_matrices),*] },
                split_link_profiles,
            )
        }
        _ => (quote! {}, quote! {}),
    };
    // The DFU updater task only attaches to a USB transport. Split centrals
    // seed the transport's passthrough alt slots from the peripheral count.
    let communication_uses_usb = matches!(
        communication,
        CommunicationConfig::Usb(_) | CommunicationConfig::Both(_, _)
    );
    let (dfu_interface, num_peripherals) = if cfg!(feature = "_dfu") && communication_uses_usb {
        let num_peripherals = match board {
            BoardConfig::Split(split) => split.peripheral.len(),
            BoardConfig::UniBody(_) => 0,
        };
        (expand_dfu_interface(hardware.dfu.as_ref()), num_peripherals)
    } else {
        (quote! {}, 0)
    };
    let (transport_prelude, transport_tasks) = transport_setup(
        host,
        communication,
        split_peripheral_matrices,
        split_link_profiles,
        &dfu_interface,
        num_peripherals,
    );

    let entry = match board {
        BoardConfig::Split(split_config) => {
            let keyboard_task = quote! {
                keyboard.run(),
            };
            let mut tasks = vec![devices_task, keyboard_task];
            tasks.extend(registered_processors);
            tasks.extend(transport_tasks);
            if let Some(t) = &watchdog_task {
                tasks.push(t.clone());
            }
            match split_config.connection {
                SplitConnection::Ble => {
                    if !processors.is_empty() {
                        tasks.push(processors_task);
                    };
                    tasks.push(quote! {
                        ::rmk::split::ble::central::run_split_connection_supervisor()
                    });
                    let joined = join_all_tasks(tasks);
                    quote! {
                        #transport_prelude
                        #auto_mouse_layer_prelude
                        #joined
                    }
                }
                SplitConnection::Serial => {
                    if !processors.is_empty() {
                        tasks.push(processors_task);
                    };
                    let central_serials = split_config
                        .central
                        .serial
                        .clone()
                        .expect("No serial defined for central");
                    split_config
                        .peripheral
                        .iter()
                        .enumerate()
                        .for_each(|(idx, p)| {
                            let row = p.rows as u8;
                            let col = p.cols as u8;
                            let row_offset = p.row_offset as u8;
                            let col_offset = p.col_offset as u8;
                            let uart_instance =
                                format_ident!(
                            "{}",
                            central_serials
                                .get(idx)
                                .expect("No or not enough serial defined for peripheral in central")
                                .instance
                                .to_lowercase()
                        );
                            let rmk_features = get_rmk_features();
                            let dfu_split_enabled = is_feature_enabled(&rmk_features, "dfu_split");
                            let policy = if dfu_split_enabled {
                                match p.update_policy {
                                    Some(rmk_config::UpdatePolicy::Force) => {
                                        quote! { ::rmk::split::central::UpdatePolicy::Force }
                                    }
                                    _ => quote! { ::rmk::split::central::UpdatePolicy::MatchHash },
                                }
                            } else {
                                quote! {}
                            };
                            tasks.push(quote! {
                                ::rmk::split::central::run_peripheral_manager(
                                    #idx,
                                    #uart_instance,
                                    ::rmk::split::PeripheralMatrixConfig {
                                        rows: #row,
                                        cols: #col,
                                        row_offset: #row_offset,
                                        col_offset: #col_offset,
                                    },
                                    #policy
                                )
                            });
                        });

                    let joined = join_all_tasks(tasks);
                    quote! {
                        #transport_prelude
                        #auto_mouse_layer_prelude
                        #joined
                    }
                }
            }
        }
        BoardConfig::UniBody(_) => rmk_entry_unibody(
            transport_prelude,
            auto_mouse_layer_prelude,
            transport_tasks,
            devices_task,
            processors_task,
            registered_processors,
            watchdog_task,
        ),
    };

    quote! {
        use ::rmk::core_traits::Runnable;
        #entry
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn rmk_entry_unibody(
    transport_prelude: TokenStream2,
    auto_mouse_layer_prelude: Option<TokenStream2>,
    transport_tasks: Vec<TokenStream2>,
    devices_task: TokenStream2,
    processors_task: TokenStream2,
    registered_processors: Vec<TokenStream2>,
    watchdog_task: Option<TokenStream2>,
) -> TokenStream2 {
    let keyboard_task = quote! {
        keyboard.run()
    };

    let mut tasks = vec![devices_task, keyboard_task];
    if !processors_task.is_empty() {
        tasks.push(processors_task);
    }
    tasks.extend(registered_processors);
    tasks.extend(transport_tasks);
    if let Some(t) = watchdog_task {
        tasks.push(t);
    }
    let joined = join_all_tasks(tasks);
    quote! {
        #transport_prelude
        #auto_mouse_layer_prelude
        #joined
    }
}

/// Build (`let transport = ...;` prelude, transport run tasks) for the active
/// communication config.
fn transport_setup(
    host: &Host,
    communication: &CommunicationConfig,
    split_peripheral_matrices: TokenStream2,
    split_link_profiles: TokenStream2,
    dfu_interface: &TokenStream2,
    num_peripherals: usize,
) -> (TokenStream2, Vec<TokenStream2>) {
    let wpm_prelude = quote! {
        let mut wpm_processor = ::rmk::processor::builtin::wpm::WpmProcessor::new();
    };
    let wpm_task = quote! { wpm_processor.run() };

    let host_active = host.vial_enabled || host.rynk_enabled;

    let with_host = if host_active {
        quote! { .with_host_service(&host_service) }
    } else {
        quote! {}
    };

    let dfu_split_enabled = is_feature_enabled(&get_rmk_features(), "dfu_split");
    let usb_device_config = if matches!(communication, CommunicationConfig::Both(_, _)) {
        quote! { rmk_config.device_config.clone() }
    } else {
        quote! { rmk_config.device_config }
    };
    let dfu_split_arg =
        (dfu_split_enabled && !dfu_interface.is_empty()).then(|| quote! { , #num_peripherals });
    let usb_prelude = quote! {
        #dfu_interface
        let mut usb_transport = ::rmk::usb::UsbTransport::new(driver, #usb_device_config #dfu_split_arg)#with_host;
    };
    // Boards may supply host-link power management through a config function.
    let host_power_config = std::env::var("RMK_BLE_HOST_POWER_CONFIG_FN")
        .ok()
        .and_then(|path| syn::parse_str::<syn::Path>(&path).ok())
        .map(|path| quote! { .with_host_power_config(#path()) });
    let ble_prelude = quote! {
        let mut ble_transport = ::rmk::ble::BleTransport::new(ble_controller, ble_addr, rmk_config #split_peripheral_matrices) #with_host #host_power_config #split_link_profiles;
    };
    let ble_task = quote! { ble_transport.run() };

    // The DFU updater task runs alongside the transport it feeds.
    let dfu_task = if dfu_interface.is_empty() {
        quote! {}
    } else {
        quote! { dfu_iface.run() }
    };

    match communication {
        CommunicationConfig::Usb(_) => {
            let prelude = quote! {
                #wpm_prelude
                #usb_prelude
            };
            let mut tasks = vec![quote! { usb_transport.run() }, wpm_task];
            if !dfu_task.is_empty() {
                tasks.push(dfu_task);
            }
            (prelude, tasks)
        }
        CommunicationConfig::Ble(_) => {
            let prelude = quote! {
                #wpm_prelude
                #ble_prelude
            };
            (prelude, vec![ble_task, wpm_task])
        }
        CommunicationConfig::Both(_, _) => {
            let prelude = quote! {
                #wpm_prelude
                #usb_prelude
                #ble_prelude
            };
            let mut tasks = vec![quote! { usb_transport.run() }, ble_task, wpm_task];
            if !dfu_task.is_empty() {
                tasks.insert(1, dfu_task);
            }
            (prelude, tasks)
        }
        CommunicationConfig::None => panic!("USB and BLE are both disabled"),
    }
}

pub(crate) fn expand_tasks(mut tasks: Vec<TokenStream2>) -> TokenStream2 {
    match tasks.len() {
        0 => quote! {},
        1 => {
            // Task producers may include an argument-list trailing comma.
            let mut tokens: Vec<_> = tasks.pop().unwrap().into_iter().collect();
            if matches!(tokens.last(), Some(proc_macro2::TokenTree::Punct(p)) if p.as_char() == ',')
            {
                tokens.pop();
            }
            tokens.into_iter().collect()
        }
        count => {
            // A left-deep Join keeps large nested poll frames live together.
            // Balance the tree while retaining left-to-right polling order.
            let right = tasks.split_off(count / 2);
            let left = expand_tasks(tasks);
            let right = expand_tasks(right);
            quote! { ::rmk::embassy_futures::join::join(#left, #right) }
        }
    }
}

pub(crate) fn join_all_tasks(tasks: Vec<TokenStream2>) -> TokenStream2 {
    let joined = expand_tasks(tasks);
    quote! {
        #joined.await;
    }
}

#[cfg(test)]
mod task_join_tests {
    use super::*;

    fn inspect_tree(expr: &syn::Expr, leaves: &mut Vec<String>) -> usize {
        match expr {
            syn::Expr::Call(call) => {
                let syn::Expr::Path(path) = &*call.func else {
                    panic!("expected join path")
                };
                assert_eq!(path.path.segments.last().unwrap().ident, "join");
                assert_eq!(call.args.len(), 2);
                1 + call
                    .args
                    .iter()
                    .map(|arg| inspect_tree(arg, leaves))
                    .max()
                    .unwrap()
            }
            syn::Expr::Path(path) => {
                leaves.push(path.path.get_ident().unwrap().to_string());
                0
            }
            _ => panic!("unexpected generated expression"),
        }
    }

    #[test]
    fn task_expressions_with_trailing_commas_remain_valid_when_balanced() {
        let tasks = vec![
            quote! { task_0, },
            quote! { task_1 },
            quote! { task_2, },
            quote! { task_3 },
        ];
        let tree: syn::Expr =
            syn::parse2(expand_tasks(tasks)).expect("valid nested join expression");
        let mut leaves = Vec::new();
        assert_eq!(inspect_tree(&tree, &mut leaves), 2);
        assert_eq!(leaves, ["task_0", "task_1", "task_2", "task_3"]);
    }

    #[test]
    fn many_tasks_use_bounded_join_depth_without_reordering_or_dropping_tasks() {
        for count in 2usize..=32 {
            let names: Vec<_> = (0..count).map(|i| format_ident!("task_{i}")).collect();
            let tasks = names.iter().map(|name| quote! { #name }).collect();
            let tree: syn::Expr = syn::parse2(expand_tasks(tasks)).unwrap();
            let mut leaves = Vec::new();
            let depth = inspect_tree(&tree, &mut leaves);
            assert_eq!(
                leaves,
                names.iter().map(ToString::to_string).collect::<Vec<_>>()
            );
            let limit = (usize::BITS - (count - 1).leading_zeros()) as usize;
            assert!(
                depth <= limit,
                "{count} tasks: depth {depth} exceeds {limit}"
            );
        }
    }
}
