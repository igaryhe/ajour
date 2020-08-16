use {
    super::{Ajour, AjourState, Interaction, Message},
    crate::{
        addon::{Addon, AddonState},
        config::load_config,
        curse_api,
        error::ClientError,
        fs::{delete_addon, install_addon},
        network::download_addon,
        toc::read_addon_directory,
        tukui_api, wowinterface_api, Result,
    },
    iced::Command,
    std::path::PathBuf,
};

pub fn handle_message(ajour: &mut Ajour, message: Message) -> Result<Command<Message>> {
    match message {
        Message::Parse(config) => {
            // When we have the config, we parse the addon directory
            // which is provided by the config.
            ajour.config = config;
            let addon_directory = ajour.config.get_addon_directory();

            match addon_directory {
                Some(dir) => {
                    return Ok(Command::perform(
                        read_addon_directory(dir),
                        Message::PatchAddons,
                    ))
                }
                None => {
                    return Err(ClientError::Custom(
                        "World of Warcraft directory is not set.".to_owned(),
                    ))
                }
            }
        }
        Message::Interaction(Interaction::Refresh) => {
            // Re-parse addons.
            ajour.state = AjourState::Loading;
            ajour.addons = Vec::new();
            return Ok(Command::perform(load_config(), Message::Parse));
        }
        Message::Interaction(Interaction::Delete(id)) => {
            // Delete addon, and it's dependencies.
            // TODO: maybe just rewrite and assume it goes well and remove addon.
            let addons = &ajour.addons.clone();
            let target_addon = addons.iter().find(|a| a.id == id).unwrap();
            let combined_dependencies = target_addon.combined_dependencies(addons);
            let addons_to_be_deleted = addons
                .iter()
                .filter(|a| combined_dependencies.contains(&a.id))
                .collect::<Vec<_>>();

            // Loops the addons marked for deletion and remove them one by one.
            for addon in addons_to_be_deleted {
                let _ = delete_addon(addon);
            }
            // Refreshes the GUI by re-parsing the addon directory.
            // TODO: This can be done prettier.
            let addon_directory = ajour.config.get_addon_directory().unwrap();
            return Ok(Command::perform(
                read_addon_directory(addon_directory),
                Message::PatchAddons,
            ));
        }
        Message::Interaction(Interaction::Update(id)) => {
            let to_directory = ajour
                .config
                .get_temporary_addon_directory()
                .expect("Expected a valid path");
            for addon in &mut ajour.addons {
                if addon.id == id {
                    addon.state = AddonState::Downloading;
                    return Ok(Command::perform(
                        perform_download_addon(addon.clone(), to_directory),
                        Message::DownloadedAddon,
                    ));
                }
            }
        }
        Message::Interaction(Interaction::UpdateAll) => {
            // Update all pressed
            let mut commands = Vec::<Command<Message>>::new();
            for addon in &mut ajour.addons {
                if addon.state == AddonState::Updatable {
                    let to_directory = ajour
                        .config
                        .get_temporary_addon_directory()
                        .expect("Expected a valid path");
                    addon.state = AddonState::Downloading;
                    let addon = addon.clone();
                    commands.push(Command::perform(
                        perform_download_addon(addon, to_directory),
                        Message::DownloadedAddon,
                    ))
                }
            }
            return Ok(Command::batch(commands));
        }
        Message::PatchAddons(Ok(addons)) => {
            ajour.addons = addons;

            let mut commands = Vec::<Command<Message>>::new();
            let addons = ajour.addons.clone();
            for addon in addons {
                // TODO: filter this instead of this if.
                if addon.is_parent() {
                    if let (Some(_), Some(token)) =
                        (&addon.wowi_id, &ajour.config.tokens.wowinterface)
                    {
                        commands.push(Command::perform(
                            fetch_wowinterface_packages(addon, token.to_string()),
                            Message::WowinterfacePackages,
                        ))
                    } else if let Some(_) = &addon.tukui_id {
                        commands.push(Command::perform(
                            fetch_tukui_package(addon),
                            Message::TukuiPackage,
                        ))
                    } else if let Some(_) = &addon.curse_id {
                        commands.push(Command::perform(
                            fetch_curse_package(addon),
                            Message::CursePackage,
                        ))
                    } else {
                        commands.push(Command::perform(
                            fetch_curse_packages(addon),
                            Message::CursePackages,
                        ))
                    }
                }
            }

            return Ok(Command::batch(commands));
        }
        Message::CursePackage((id, result)) => {
            if let Ok(package) = result {
                let addon = ajour
                    .addons
                    .iter_mut()
                    .find(|a| a.id == id)
                    .expect("Expected addon for id to exist.");
                addon.apply_curse_package(&package, &ajour.config.wow.flavor);
            }
        }
        Message::CursePackages((id, result)) => {
            if let Ok(packages) = result {
                let addon = ajour
                    .addons
                    .iter_mut()
                    .find(|a| a.id == id)
                    .expect("Expected addon for id to exist.");
                addon.apply_curse_packages(&packages, &ajour.config.wow.flavor);
            }
        }
        Message::TukuiPackage((id, result)) => {
            if let Ok(package) = result {
                let addon = ajour
                    .addons
                    .iter_mut()
                    .find(|a| a.id == id)
                    .expect("Expected addon for id to exist.");
                addon.apply_tukui_package(&package);
            }
        }
        Message::WowinterfacePackages((id, result)) => {
            if let Ok(packages) = result {
                let addon = ajour
                    .addons
                    .iter_mut()
                    .find(|a| a.id == id)
                    .expect("Expected addon for id to exist.");
                addon.apply_wowi_packages(&packages);
            }
        }
        Message::DownloadedAddon((id, result)) => {
            // When an addon has been successfully downloaded we begin to
            // unpack it.
            // If it for some reason fails to download, we handle the error.
            let from_directory = ajour
                .config
                .get_temporary_addon_directory()
                .expect("Expected a valid path");
            let to_directory = ajour
                .config
                .get_addon_directory()
                .expect("Expected a valid path");
            let addon = ajour
                .addons
                .iter_mut()
                .find(|a| a.id == id)
                .expect("Expected addon for id to exist.");
            match result {
                Ok(_) => {
                    if addon.state == AddonState::Downloading {
                        addon.state = AddonState::Unpacking;
                        let addon = addon.clone();
                        return Ok(Command::perform(
                            perform_unpack_addon(addon, from_directory, to_directory),
                            Message::UnpackedAddon,
                        ));
                    }
                }
                Err(err) => {
                    ajour.state = AjourState::Error(err);
                }
            }
        }
        Message::UnpackedAddon((id, result)) => {
            let addon = ajour
                .addons
                .iter_mut()
                .find(|a| a.id == id)
                .expect("Expected addon for id to exist.");
            match result {
                Ok(_) => {
                    addon.state = AddonState::Ajour(Some("Completed".to_owned()));
                    addon.version = addon.remote_version.clone();
                }
                Err(err) => {
                    // TODO: Handle when addon fails to unpack.
                    ajour.state = AjourState::Error(err);
                    addon.state = AddonState::Ajour(Some("Error!".to_owned()));
                }
            }
        }
        Message::Error(error) | Message::PatchAddons(Err(error)) => {
            ajour.state = AjourState::Error(error);
        }
    }

    Ok(Command::none())
}

async fn fetch_curse_package(addon: Addon) -> (String, Result<curse_api::Package>) {
    (
        addon.id.clone(),
        curse_api::fetch_remote_package(
            &addon.curse_id.expect("Expected to have curse_id on Addon."),
        )
        .await,
    )
}

async fn fetch_curse_packages(addon: Addon) -> (String, Result<Vec<curse_api::Package>>) {
    (
        addon.id.clone(),
        curse_api::fetch_remote_packages(&addon.title).await,
    )
}

async fn fetch_tukui_package(addon: Addon) -> (String, Result<tukui_api::Package>) {
    (
        addon.id.clone(),
        tukui_api::fetch_remote_package(
            &addon.tukui_id.expect("Expected to have tukui_id on Addon."),
        )
        .await,
    )
}

async fn fetch_wowinterface_packages(
    addon: Addon,
    token: String,
) -> (String, Result<Vec<wowinterface_api::Package>>) {
    (
        addon.id.clone(),
        wowinterface_api::fetch_remote_packages(
            &addon
                .wowi_id
                .expect("Expected to have wowinterface_id on Addon."),
            &token,
        )
        .await,
    )
}

/// Downloads the newest version of the addon.
/// This is for now only downloading from warcraftinterface.
async fn perform_download_addon(addon: Addon, to_directory: PathBuf) -> (String, Result<()>) {
    (
        addon.id.clone(),
        download_addon(&addon, &to_directory).await.map(|_| ()),
    )
}

/// Unzips `Addon` at given `from_directory` and moves it `to_directory`.
async fn perform_unpack_addon(
    addon: Addon,
    from_directory: PathBuf,
    to_directory: PathBuf,
) -> (String, Result<()>) {
    (
        addon.id.clone(),
        install_addon(&addon, &from_directory, &to_directory)
            .await
            .map(|_| ()),
    )
}
