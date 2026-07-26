#[cfg(feature = "files")]
pub mod files;
#[cfg(feature = "network")]
pub mod network;
#[cfg(feature = "nfs")]
pub mod nfs;
#[cfg(feature = "podman")]
pub mod podman;
#[cfg(feature = "quadlets")]
pub mod quadlets;
#[cfg(feature = "recipes")]
pub mod recipes;
#[cfg(feature = "reverse-proxy")]
pub mod reverse_proxy;
#[cfg(feature = "samba")]
pub mod samba;
#[cfg(feature = "selinux")]
pub mod selinux;
#[cfg(feature = "services")]
pub mod services;
// `settings` is always compiled — it provides the always-available `get_system`
// action that other modules and the dashboard rely on for host identification.
pub mod settings;
#[cfg(feature = "storage")]
pub mod storage;
#[cfg(feature = "users")]
pub mod users;
#[cfg(feature = "virtual-machines")]
pub mod virtual_machines;

use super::Dispatcher;

/// Build the dispatcher with every module enabled by the active Cargo features.
///
/// Order matters only for the `agent.capabilities` response, which lists
/// modules in `BTreeMap` name order regardless of insertion order — so this
/// function is free to list modules in whatever order reads cleanly.
#[must_use] 
pub fn default_dispatcher() -> Dispatcher {
    let dispatcher = Dispatcher::new().with_module(settings::SettingsModule);

    #[cfg(feature = "services")]
    let dispatcher = dispatcher.with_module(services::ServicesModule);

    #[cfg(feature = "files")]
    let dispatcher = dispatcher.with_module(files::FileModule);

    #[cfg(feature = "recipes")]
    let dispatcher = dispatcher.with_module(recipes::RecipeModule);

    #[cfg(feature = "storage")]
    let dispatcher = dispatcher.with_module(storage::StorageModule);

    #[cfg(feature = "samba")]
    let dispatcher = dispatcher.with_module(samba::SambaModule);

    #[cfg(feature = "selinux")]
    let dispatcher = dispatcher.with_module(selinux::SelinuxModule);

    #[cfg(feature = "nfs")]
    let dispatcher = dispatcher.with_module(nfs::NfsModule);

    #[cfg(feature = "network")]
    let dispatcher = dispatcher.with_module(network::NetworkModule);

    #[cfg(feature = "podman")]
    let dispatcher = dispatcher.with_module(podman::PodmanModule);

    #[cfg(feature = "quadlets")]
    let dispatcher = dispatcher.with_module(quadlets::QuadletsModule);

    #[cfg(feature = "reverse-proxy")]
    let dispatcher = dispatcher.with_module(reverse_proxy::ReverseProxyModule);

    #[cfg(feature = "users")]
    let dispatcher = dispatcher.with_module(users::UsersModule);

    #[cfg(feature = "virtual-machines")]
    let dispatcher = dispatcher.with_module(virtual_machines::VirtualMachinesModule);

    dispatcher
}
