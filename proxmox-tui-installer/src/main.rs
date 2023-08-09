use std::{
    collections::HashMap,
    env,
    io::{BufRead, BufReader, Write},
    net::IpAddr,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use cursive::{
    event::Event,
    theme::{ColorStyle, Effect, PaletteColor, Style},
    utils::Counter,
    view::{Nameable, Offset, Resizable, ViewWrapper},
    views::{
        Button, Checkbox, Dialog, DummyView, EditView, Layer, LinearLayout, PaddedView, Panel,
        ProgressBar, ResizedView, ScrollView, SelectView, StackView, TextContent, TextView,
        ViewRef,
    },
    Cursive, CursiveRunnable, ScreenId, View, XY,
};

use regex::Regex;

mod options;
use options::*;

mod setup;
use setup::{InstallConfig, LocaleInfo, ProxmoxProduct, RuntimeInfo, SetupInfo};

mod system;

mod utils;
use utils::Fqdn;

mod views;
use views::{
    BootdiskOptionsView, CidrAddressEditView, FormView, TableView, TableViewItem,
    TimezoneOptionsView,
};

// TextView::center() seems to garble the first two lines, so fix it manually here.
const PROXMOX_LOGO: &str = r#"
 ____
|  _ \ _ __ _____  ___ __ ___   _____  __
| |_) | '__/ _ \ \/ / '_ ` _ \ / _ \ \/ /
|  __/| | | (_) >  <| | | | | | (_) >  <
|_|   |_|  \___/_/\_\_| |_| |_|\___/_/\_\ "#;

/// ISO information is available globally.
static mut SETUP_INFO: Option<SetupInfo> = None;

pub fn setup_info() -> &'static SetupInfo {
    unsafe { SETUP_INFO.as_ref().unwrap() }
}

fn init_setup_info(info: SetupInfo) {
    unsafe {
        SETUP_INFO = Some(info);
    }
}

#[inline]
pub fn current_product() -> setup::ProxmoxProduct {
    setup_info().config.product
}

struct InstallerView {
    view: ResizedView<Dialog>,
}

impl InstallerView {
    pub fn new<T: View>(
        state: &InstallerState,
        view: T,
        next_cb: Box<dyn Fn(&mut Cursive)>,
        focus_next: bool,
    ) -> Self {
        let mut bbar = LinearLayout::horizontal()
            .child(abort_install_button())
            .child(DummyView.full_width())
            .child(Button::new("Previous", switch_to_prev_screen))
            .child(DummyView)
            .child(Button::new("Next", next_cb));
        let _ = bbar.set_focus_index(4); // ignore errors
        let mut inner = LinearLayout::vertical()
            .child(PaddedView::lrtb(0, 0, 1, 1, view))
            .child(PaddedView::lrtb(1, 1, 0, 0, bbar));
        if focus_next {
            let _ = inner.set_focus_index(1); // ignore errors
        }

        Self::with_raw(state, inner)
    }

    pub fn with_raw(state: &InstallerState, view: impl View) -> Self {
        let setup = &state.setup_info;

        let title = format!(
            "{} ({}-{}) Installer",
            setup.config.fullname, setup.iso_info.release, setup.iso_info.isorelease
        );

        let inner = Dialog::around(view).title(title);

        Self {
            // Limit the maximum to something reasonable, such that it won't get spread out much
            // depending on the screen.
            view: ResizedView::with_max_size((120, 40), inner),
        }
    }
}

impl ViewWrapper for InstallerView {
    cursive::wrap_impl!(self.view: ResizedView<Dialog>);
}

struct InstallerBackgroundView {
    view: StackView,
}

impl InstallerBackgroundView {
    pub fn new() -> Self {
        let style = Style {
            effects: Effect::Bold.into(),
            color: ColorStyle::back(PaletteColor::View),
        };

        let mut view = StackView::new();
        view.add_fullscreen_layer(Layer::with_color(
            DummyView
                .full_width()
                .fixed_height(PROXMOX_LOGO.lines().count() + 1),
            ColorStyle::back(PaletteColor::View),
        ));
        view.add_transparent_layer_at(
            XY {
                x: Offset::Center,
                y: Offset::Absolute(0),
            },
            TextView::new(PROXMOX_LOGO).style(style),
        );

        Self { view }
    }
}

impl ViewWrapper for InstallerBackgroundView {
    cursive::wrap_impl!(self.view: StackView);
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum InstallerStep {
    Licence,
    Bootdisk,
    Timezone,
    Password,
    Network,
    Summary,
    Install,
}

#[derive(Clone)]
struct InstallerState {
    options: InstallerOptions,
    /// FIXME: Remove:
    setup_info: SetupInfo,
    runtime_info: RuntimeInfo,
    locales: LocaleInfo,
    steps: HashMap<InstallerStep, ScreenId>,
    in_test_mode: bool,
}

fn main() {
    let mut siv = cursive::termion();

    let in_test_mode = match env::args().nth(1).as_deref() {
        Some("-t") => true,

        // Always force the test directory in debug builds
        _ => cfg!(debug_assertions),
    };

    let (locales, runtime_info) = match installer_setup(in_test_mode) {
        Ok(result) => result,
        Err(err) => initial_setup_error(&mut siv, &err),
    };

    siv.clear_global_callbacks(Event::CtrlChar('c'));
    siv.set_on_pre_event(Event::CtrlChar('c'), trigger_abort_install_dialog);

    siv.set_user_data(InstallerState {
        options: InstallerOptions {
            bootdisk: BootdiskOptions::defaults_from(&runtime_info.disks[0]),
            timezone: TimezoneOptions::defaults_from(&runtime_info, &locales),
            password: Default::default(),
            network: NetworkOptions::from(&runtime_info.network),
            autoreboot: false,
        },
        setup_info: setup_info().clone(), // FIXME: REMOVE
        runtime_info,
        locales,
        steps: HashMap::new(),
        in_test_mode,
    });

    switch_to_next_screen(&mut siv, InstallerStep::Licence, &license_dialog);
    siv.run();
}

fn installer_setup(in_test_mode: bool) -> Result<(LocaleInfo, RuntimeInfo), String> {
    let base_path = if in_test_mode { "./testdir" } else { "/" };
    let mut path = PathBuf::from(base_path);

    path.push("run");
    path.push("proxmox-installer");

    let installer_info = {
        let mut path = path.clone();
        path.push("iso-info.json");

        setup::read_json(&path).map_err(|err| format!("Failed to retrieve setup info: {err}"))?
    };
    init_setup_info(installer_info);

    let locale_info = {
        let mut path = path.clone();
        path.push("locales.json");

        setup::read_json(&path).map_err(|err| format!("Failed to retrieve locale info: {err}"))?
    };

    let mut runtime_info: RuntimeInfo = {
        let mut path = path.clone();
        path.push("run-env-info.json");

        setup::read_json(&path)
            .map_err(|err| format!("Failed to retrieve runtime environment info: {err}"))?
    };

    runtime_info.disks.sort();
    if runtime_info.disks.is_empty() {
        Err("The installer could not find any supported hard disks.".to_owned())
    } else {
        Ok((locale_info, runtime_info))
    }
}

/// Anything that can be done late in the setup and will not result in fatal errors.
fn installer_setup_late(siv: &mut Cursive) {
    let state = siv.user_data::<InstallerState>().cloned().unwrap();

    if !state.in_test_mode {
        let kmap_id = &state.options.timezone.kb_layout;
        if let Some(kmap) = state.locales.kmap.get(kmap_id) {
            if let Err(err) = system::set_keyboard_layout(kmap) {
                display_setup_warning(siv, &format!("Failed to apply keyboard layout: {err}"));
            }
        }
    }

    if state.runtime_info.total_memory < 1024 {
        display_setup_warning(
            siv,
            concat!(
                "Less than 1 GiB of usable memory detected, installation will probably fail.\n\n",
                "See 'System Requirements' in the documentation."
            ),
        );
    }

    if state.setup_info.config.product == ProxmoxProduct::PVE && !state.runtime_info.hvm_supported {
        display_setup_warning(
            siv,
            concat!(
                "No support for hardware-accelerated KVM virtualization detected.\n\n",
                "Check BIOS settings for Intel VT / AMD-V / SVM."
            ),
        );
    }
}

fn initial_setup_error(siv: &mut CursiveRunnable, message: &str) -> ! {
    siv.add_layer(
        Dialog::around(TextView::new(message))
            .title("Installer setup error")
            .button("Ok", Cursive::quit),
    );
    siv.run();

    std::process::exit(1);
}

fn display_setup_warning(siv: &mut Cursive, message: &str) {
    siv.add_layer(Dialog::info(message).title("Warning"));
}

fn switch_to_next_screen(
    siv: &mut Cursive,
    step: InstallerStep,
    constructor: &dyn Fn(&mut Cursive) -> InstallerView,
) {
    let state = siv.user_data::<InstallerState>().cloned().unwrap();
    let is_first_screen = state.steps.is_empty();

    // Check if the screen already exists; if yes, then simply switch to it.
    if let Some(screen_id) = state.steps.get(&step) {
        siv.set_screen(*screen_id);

        // The summary view cannot be cached (otherwise it would display stale values). Thus
        // replace it if the screen is switched to.
        // TODO: Could be done by e.g. having all the main dialog views implement some sort of
        // .refresh(), which can be called if the view is switched to.
        if step == InstallerStep::Summary {
            let view = constructor(siv);
            siv.screen_mut().pop_layer();
            siv.screen_mut().add_layer(view);
        }

        return;
    }

    let v = constructor(siv);
    let screen = siv.add_active_screen();
    siv.with_user_data(|state: &mut InstallerState| state.steps.insert(step, screen));

    siv.screen_mut().add_transparent_layer_at(
        XY {
            x: Offset::Parent(0),
            y: Offset::Parent(0),
        },
        InstallerBackgroundView::new(),
    );

    siv.screen_mut().add_layer(v);

    // If this is the first screen to be added, execute our late setup first.
    // Needs to be done here, at the end, to ensure that any potential layers get added to
    // the right screen and are on top.
    if is_first_screen {
        installer_setup_late(siv);
    }
}

fn switch_to_prev_screen(siv: &mut Cursive) {
    let id = siv.active_screen().saturating_sub(1);
    siv.set_screen(id);
}

fn yes_no_dialog(
    siv: &mut Cursive,
    title: &str,
    text: &str,
    callback_yes: Box<dyn Fn(&mut Cursive)>,
    callback_no: Box<dyn Fn(&mut Cursive)>,
) {
    siv.add_layer(
        Dialog::around(TextView::new(text))
            .title(title)
            .button("No", move |siv| {
                siv.pop_layer();
                callback_no(siv);
            })
            .button("Yes", move |siv| {
                siv.pop_layer();
                callback_yes(siv);
            }),
    )
}

fn trigger_abort_install_dialog(siv: &mut Cursive) {
    #[cfg(debug_assertions)]
    siv.quit();

    #[cfg(not(debug_assertions))]
    yes_no_dialog(
        siv,
        "Abort installation?",
        "Are you sure you want to abort the installation?",
        Box::new(Cursive::quit),
        Box::new(|_| {}),
    )
}

fn abort_install_button() -> Button {
    Button::new("Abort", trigger_abort_install_dialog)
}

fn get_eula(setup: &SetupInfo) -> String {
    let mut path = setup.locations.iso.clone();
    path.push("EULA");

    std::fs::read_to_string(path)
        .unwrap_or_else(|_| "< Debug build - ignoring non-existing EULA >".to_owned())
}

fn license_dialog(siv: &mut Cursive) -> InstallerView {
    let state = siv.user_data::<InstallerState>().unwrap();

    let mut bbar = LinearLayout::horizontal()
        .child(abort_install_button())
        .child(DummyView.full_width())
        .child(Button::new("I agree", |siv| {
            switch_to_next_screen(siv, InstallerStep::Bootdisk, &bootdisk_dialog)
        }));
    let _ = bbar.set_focus_index(2); // ignore errors

    let mut inner = LinearLayout::vertical()
        .child(PaddedView::lrtb(
            0,
            0,
            1,
            0,
            TextView::new("END USER LICENSE AGREEMENT (EULA)").center(),
        ))
        .child(Panel::new(ScrollView::new(
            TextView::new(get_eula(&state.setup_info)).center(),
        )))
        .child(PaddedView::lrtb(1, 1, 1, 0, bbar));

    let _ = inner.set_focus_index(2); // ignore errors

    InstallerView::with_raw(state, inner)
}

fn bootdisk_dialog(siv: &mut Cursive) -> InstallerView {
    let state = siv.user_data::<InstallerState>().cloned().unwrap();

    InstallerView::new(
        &state,
        BootdiskOptionsView::new(&state.runtime_info.disks, &state.options.bootdisk)
            .with_name("bootdisk-options"),
        Box::new(|siv| {
            let options = siv.call_on_name("bootdisk-options", BootdiskOptionsView::get_values);

            match options {
                Some(Ok(options)) => {
                    siv.with_user_data(|state: &mut InstallerState| {
                        state.options.bootdisk = options;
                    });

                    switch_to_next_screen(siv, InstallerStep::Timezone, &timezone_dialog);
                }

                Some(Err(err)) => siv.add_layer(Dialog::info(format!("Invalid values: {err}"))),
                _ => siv.add_layer(Dialog::info("Invalid values")),
            }
        }),
        true,
    )
}

fn timezone_dialog(siv: &mut Cursive) -> InstallerView {
    let state = siv.user_data::<InstallerState>().unwrap();
    let options = &state.options.timezone;

    InstallerView::new(
        state,
        TimezoneOptionsView::new(&state.locales, options).with_name("timezone-options"),
        Box::new(|siv| {
            let options = siv.call_on_name("timezone-options", TimezoneOptionsView::get_values);

            match options {
                Some(Ok(options)) => {
                    siv.with_user_data(|state: &mut InstallerState| {
                        state.options.timezone = options;
                    });

                    switch_to_next_screen(siv, InstallerStep::Password, &password_dialog);
                }
                Some(Err(err)) => siv.add_layer(Dialog::info(format!("Invalid values: {err}"))),
                _ => siv.add_layer(Dialog::info("Invalid values")),
            }
        }),
        true,
    )
}

fn password_dialog(siv: &mut Cursive) -> InstallerView {
    let state = siv.user_data::<InstallerState>().unwrap();
    let options = &state.options.password;

    let inner = FormView::new()
        .child("Root password", EditView::new().secret())
        .child("Confirm root password", EditView::new().secret())
        .child(
            "Administator email",
            EditView::new().content(&options.email),
        )
        .with_name("password-options");

    InstallerView::new(
        state,
        inner,
        Box::new(|siv| {
            let options = siv.call_on_name("password-options", |view: &mut FormView| {
                let root_password = view
                    .get_value::<EditView, _>(0)
                    .ok_or("failed to retrieve password")?;

                let confirm_password = view
                    .get_value::<EditView, _>(1)
                    .ok_or("failed to retrieve password confirmation")?;

                let email = view
                    .get_value::<EditView, _>(2)
                    .ok_or("failed to retrieve email")?;

                let email_regex =
                    Regex::new(r"^[\w\+\-\~]+(\.[\w\+\-\~]+)*@[a-zA-Z0-9\-]+(\.[a-zA-Z0-9\-]+)*$")
                        .unwrap();

                if root_password.len() < 5 {
                    Err("password too short, must be at least 5 characters long")
                } else if root_password != confirm_password {
                    Err("passwords do not match")
                } else if email == "mail@example.invalid" {
                    Err("invalid email address")
                } else if !email_regex.is_match(&email) {
                    Err("Email does not look like a valid address (user@domain.tld)")
                } else {
                    Ok(PasswordOptions {
                        root_password,
                        email,
                    })
                }
            });

            match options {
                Some(Ok(options)) => {
                    siv.with_user_data(|state: &mut InstallerState| {
                        state.options.password = options;
                    });

                    switch_to_next_screen(siv, InstallerStep::Network, &network_dialog);
                }
                Some(Err(err)) => siv.add_layer(Dialog::info(format!("Invalid values: {err}"))),
                _ => siv.add_layer(Dialog::info("Invalid values")),
            }
        }),
        false,
    )
}

fn network_dialog(siv: &mut Cursive) -> InstallerView {
    let state = siv.user_data::<InstallerState>().unwrap();
    let options = &state.options.network;

    let inner = FormView::new()
        .child(
            "Management interface",
            SelectView::new()
                .popup()
                .with_all_str(state.runtime_info.network.interfaces.keys()),
        )
        .child(
            "Hostname (FQDN)",
            EditView::new().content(options.fqdn.to_string()),
        )
        .child(
            "IP address (CIDR)",
            CidrAddressEditView::new().content(options.address.clone()),
        )
        .child(
            "Gateway address",
            EditView::new().content(options.gateway.to_string()),
        )
        .child(
            "DNS server address",
            EditView::new().content(options.dns_server.to_string()),
        )
        .with_name("network-options");

    InstallerView::new(
        state,
        inner,
        Box::new(|siv| {
            let options = siv.call_on_name("network-options", |view: &mut FormView| {
                let ifname = view
                    .get_value::<SelectView, _>(0)
                    .ok_or("failed to retrieve management interface name")?;

                let fqdn = view
                    .get_value::<EditView, _>(1)
                    .ok_or("failed to retrieve host FQDN")?
                    .parse::<Fqdn>()
                    .map_err(|err| format!("hostname does not look valid:\n\n{err}"))?;

                let address = view
                    .get_value::<CidrAddressEditView, _>(2)
                    .ok_or("failed to retrieve host address")?;

                let gateway = view
                    .get_value::<EditView, _>(3)
                    .ok_or("failed to retrieve gateway address")?
                    .parse::<IpAddr>()
                    .map_err(|err| err.to_string())?;

                let dns_server = view
                    .get_value::<EditView, _>(4)
                    .ok_or("failed to retrieve DNS server address")?
                    .parse::<IpAddr>()
                    .map_err(|err| err.to_string())?;

                if address.addr().is_ipv4() != gateway.is_ipv4() {
                    Err("host and gateway IP address version must not differ".to_owned())
                } else if address.addr().is_ipv4() != dns_server.is_ipv4() {
                    Err("host and DNS IP address version must not differ".to_owned())
                } else if fqdn.to_string().ends_with(".invalid") {
                    Err("hostname does not look valid".to_owned())
                } else {
                    Ok(NetworkOptions {
                        ifname,
                        fqdn,
                        address,
                        gateway,
                        dns_server,
                    })
                }
            });

            match options {
                Some(Ok(options)) => {
                    siv.with_user_data(|state: &mut InstallerState| {
                        state.options.network = options;
                    });

                    switch_to_next_screen(siv, InstallerStep::Summary, &summary_dialog);
                }
                Some(Err(err)) => siv.add_layer(Dialog::info(format!("Invalid values: {err}"))),
                _ => siv.add_layer(Dialog::info("Invalid values")),
            }
        }),
        true,
    )
}

pub struct SummaryOption {
    name: &'static str,
    value: String,
}

impl SummaryOption {
    pub fn new<S: Into<String>>(name: &'static str, value: S) -> Self {
        Self {
            name,
            value: value.into(),
        }
    }
}

impl TableViewItem for SummaryOption {
    fn get_column(&self, name: &str) -> String {
        match name {
            "name" => self.name.to_owned(),
            "value" => self.value.clone(),
            _ => unreachable!(),
        }
    }
}

fn summary_dialog(siv: &mut Cursive) -> InstallerView {
    let state = siv.user_data::<InstallerState>().unwrap();

    let mut bbar = LinearLayout::horizontal()
        .child(abort_install_button())
        .child(DummyView.full_width())
        .child(Button::new("Previous", switch_to_prev_screen))
        .child(DummyView)
        .child(Button::new("Install", |siv| {
            let autoreboot = siv
                .find_name("reboot-after-install")
                .map(|v: ViewRef<Checkbox>| v.is_checked())
                .unwrap_or_default();

            siv.with_user_data(|state: &mut InstallerState| {
                state.options.autoreboot = autoreboot;
            });

            switch_to_next_screen(siv, InstallerStep::Install, &install_progress_dialog);
        }));

    let _ = bbar.set_focus_index(2); // ignore errors

    let mut inner = LinearLayout::vertical()
        .child(PaddedView::lrtb(
            0,
            0,
            1,
            2,
            TableView::new()
                .columns(&[
                    ("name".to_owned(), "Option".to_owned()),
                    ("value".to_owned(), "Selected value".to_owned()),
                ])
                .items(state.options.to_summary(&state.locales)),
        ))
        .child(
            LinearLayout::horizontal()
                .child(DummyView.full_width())
                .child(Checkbox::new().checked().with_name("reboot-after-install"))
                .child(
                    TextView::new(" Automatically reboot after successful installation").no_wrap(),
                )
                .child(DummyView.full_width()),
        )
        .child(PaddedView::lrtb(1, 1, 1, 0, bbar));

    let _ = inner.set_focus_index(2); // ignore errors

    InstallerView::with_raw(state, inner)
}

fn install_progress_dialog(siv: &mut Cursive) -> InstallerView {
    // Ensure the screen is updated independently of keyboard events and such
    siv.set_autorefresh(true);

    let cb_sink = siv.cb_sink().clone();
    let state = siv.user_data::<InstallerState>().unwrap();
    let progress_text = TextContent::new("starting the installation ..");

    let progress_task = {
        let progress_text = progress_text.clone();
        let options = state.options.clone();
        move |counter: Counter| {
            let child = {
                use std::process::{Command, Stdio};

                #[cfg(not(debug_assertions))]
                let (path, args, envs): (&str, [&str; 1], [(&str, &str); 0]) =
                    ("proxmox-low-level-installer", ["start-session"], []);

                #[cfg(debug_assertions)]
                let (path, args, envs) = (
                    PathBuf::from("./proxmox-low-level-installer"),
                    ["-t", "start-session-test"],
                    [("PERL5LIB", ".")],
                );

                Command::new(path)
                    .args(args)
                    .envs(envs)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
            };

            let mut child = match child {
                Ok(child) => child,
                Err(err) => {
                    let _ = cb_sink.send(Box::new(move |siv| {
                        siv.add_layer(
                            Dialog::text(err.to_string())
                                .title("Error")
                                .button("Ok", Cursive::quit),
                        );
                    }));
                    return;
                }
            };

            let inner = || {
                let reader = child.stdout.take().map(BufReader::new)?;
                let mut writer = child.stdin.take()?;

                serde_json::to_writer(&mut writer, &InstallConfig::from(options)).unwrap();
                writeln!(writer).unwrap();

                let writer = Arc::new(Mutex::new(writer));

                for line in reader.lines() {
                    let line = match line {
                        Ok(line) => line,
                        Err(_) => break,
                    };

                    let msg = match line.parse::<UiMessage>() {
                        Ok(msg) => msg,
                        Err(stray) => {
                            eprintln!("low-level installer: {stray}");
                            continue;
                        }
                    };

                    match msg {
                        UiMessage::Info(s) => cb_sink.send(Box::new(|siv| {
                            siv.add_layer(Dialog::info(s).title("Information"));
                        })),
                        UiMessage::Error(s) => cb_sink.send(Box::new(|siv| {
                            siv.add_layer(Dialog::info(s).title("Error"));
                        })),
                        UiMessage::Prompt(s) => cb_sink.send({
                            let writer = writer.clone();
                            Box::new(move |siv| {
                                yes_no_dialog(
                                    siv,
                                    "Prompt",
                                    &s,
                                    Box::new({
                                        let writer = writer.clone();
                                        move |_| {
                                            if let Ok(mut writer) = writer.lock() {
                                                let _ = writeln!(writer, "ok");
                                            }
                                        }
                                    }),
                                    Box::new(move |_| {
                                        if let Ok(mut writer) = writer.lock() {
                                            let _ = writeln!(writer);
                                        }
                                    }),
                                );
                            })
                        }),
                        UiMessage::Progress(ratio, s) => {
                            counter.set(ratio);
                            progress_text.set_content(s);
                            Ok(())
                        }
                        UiMessage::Finished(success, msg) => {
                            counter.set(100);
                            progress_text.set_content(msg.to_owned());
                            cb_sink.send(Box::new(move |siv| {
                                let title = if success { "Success" } else { "Failure" };

                                // For rebooting, we just need to quit the installer,
                                // our caller does the actual reboot.
                                siv.add_layer(
                                    Dialog::text(msg)
                                        .title(title)
                                        .button("Reboot now", Cursive::quit),
                                );

                                let autoreboot = siv
                                    .user_data::<InstallerState>()
                                    .map(|state| state.options.autoreboot)
                                    .unwrap_or_default();

                                if autoreboot && success {
                                    let cb_sink = siv.cb_sink();
                                    thread::spawn({
                                        let cb_sink = cb_sink.clone();
                                        move || {
                                            thread::sleep(Duration::from_secs(5));
                                            let _ = cb_sink.send(Box::new(Cursive::quit));
                                        }
                                    });
                                }
                            }))
                        }
                    }
                    .unwrap();
                }

                Some(())
            };

            if inner().is_none() {
                cb_sink
                    .send(Box::new(|siv| {
                        siv.add_layer(
                            Dialog::text("low-level installer exited early")
                                .title("Error")
                                .button("Exit", Cursive::quit),
                        );
                    }))
                    .unwrap();
            }
        }
    };

    let progress_bar = ProgressBar::new().with_task(progress_task).full_width();
    let inner = PaddedView::lrtb(
        1,
        1,
        1,
        1,
        LinearLayout::vertical()
            .child(PaddedView::lrtb(1, 1, 0, 0, progress_bar))
            .child(DummyView)
            .child(TextView::new_with_content(progress_text).center())
            .child(PaddedView::lrtb(
                1,
                1,
                1,
                0,
                LinearLayout::horizontal().child(abort_install_button()),
            )),
    );

    InstallerView::with_raw(state, inner)
}

enum UiMessage {
    Info(String),
    Error(String),
    Prompt(String),
    Finished(bool, String),
    Progress(usize, String),
}

impl FromStr for UiMessage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (ty, rest) = s.split_once(": ").ok_or("invalid message: no type")?;

        match ty {
            "message" => Ok(UiMessage::Info(rest.to_owned())),
            "error" => Ok(UiMessage::Error(rest.to_owned())),
            "prompt" => Ok(UiMessage::Prompt(rest.to_owned())),
            "finished" => {
                let (state, rest) = rest.split_once(", ").ok_or("invalid message: no state")?;
                Ok(UiMessage::Finished(state == "ok", rest.to_owned()))
            }
            "progress" => {
                let (percent, rest) = rest.split_once(' ').ok_or("invalid progress message")?;
                Ok(UiMessage::Progress(
                    percent
                        .parse::<f64>()
                        .map(|v| (v * 100.).floor() as usize)
                        .map_err(|err| err.to_string())?,
                    rest.to_owned(),
                ))
            }
            unknown => Err(format!("invalid message type {unknown}, rest: {rest}")),
        }
    }
}
