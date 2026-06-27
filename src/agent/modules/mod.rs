pub mod files;
pub mod recipes;
pub mod services;
pub mod settings;

use super::Dispatcher;

pub fn default_dispatcher() -> Dispatcher {
    Dispatcher::new()
        .with_module(settings::SettingsModule)
        .with_module(services::ServicesModule)
        .with_module(files::FileModule)
        .with_module(recipes::RecipeModule)
}
