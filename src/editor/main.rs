//! The main crate for the map editor.

#[macro_use] extern crate qt_extras as qt;
#[macro_use] extern crate glium;
#[macro_use] extern crate serde_derive;
extern crate serde;
extern crate toml;

extern crate dreammaker as dm;
extern crate dmm_tools;
extern crate same_file;

mod map_renderer;
mod config;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use qt::widgets;
use qt::widgets::widget::Widget;
use qt::widgets::application::Application;
use qt::widgets::file_dialog::FileDialog;
use qt::widgets::tree_widget_item::TreeWidgetItem;
use qt::gui::key_sequence::KeySequence;
use qt::core::connection::Signal;
use qt::core::slots::SlotNoArgs;
use qt::core::flags::Flags;
use qt::cpp_utils::StaticCast;
use qt::cpp_utils::static_cast_mut;

use same_file::is_same_file;

use dm::objtree::{ObjectTree, TypeRef};
use config::Config;

// ----------------------------------------------------------------------------
// State layout

#[derive(Debug)]
struct State {
    widgets: EditorWindow,
    config: Config,
    env: Option<Environment>,
    maps: Vec<Map>,
    current_map: usize,
}

#[derive(Debug)]
struct Environment {
    dme: PathBuf,
    root: PathBuf,
    objtree: dm::objtree::ObjectTree,
}

#[derive(Debug)]
struct Map {
    path: PathBuf,
    dmm: dmm_tools::dmm::Map,
}

macro_rules! widget_defs {
    ($name:ident { $($field:ident: $typ:ty,)* }) => {
        #[derive(Debug)]
        struct $name {
            $($field: *mut $typ,)*
        }

        impl Default for $name {
            fn default() -> Self {
                $name {
                    $($field: 0 as *mut $typ,)*
                }
            }
        }

        impl $name {
            $(unsafe fn $field(&self) -> &mut $typ {
                &mut *self.$field
            })*
        }
    }
}

widget_defs! { EditorWindow {
    window: widgets::main_window::MainWindow,
    status_bar: widgets::status_bar::StatusBar,

    menu_file: widgets::menu::Menu,
    menu_recent: widgets::menu::Menu,

    tree: widgets::tree_widget::TreeWidget,
    map_tabs: widgets::tab_bar::TabBar,
}}

macro_rules! action {
    (@[$it:ident] (tip = $text:expr)) => {
        $it.set_status_tip(&qstr!($text));
    };
    (@[$it:ident] (key = $(^$m:ident)* $k:ident)) => {
        $it.set_shortcut(&KeySequence::new( qt::core::qt::Key::$k as i32 $(+ qt::core::qt::Modifier::$m as i32)* ));
    };
    (@[$it:ident] (slot = $slot:expr)) => {
        $it.signals().triggered().connect(&$slot);
    };
    (@[$it:ident] (disabled)) => {
        $it.set_disabled(true);
    };
    (@[$it:ident] $closure:block) => {
        let slot = SlotNoArgs::new(|| $closure);
        $it.signals().triggered().connect(&slot);
    };
    ($add_to:expr, $name:expr $(, $x:tt)*) => {
        let _it = &mut *$add_to.add_action(qstr!($name));
        $(action!(@[_it] $x);)*
    }
}

// ----------------------------------------------------------------------------
// Main window layout and core signals/slots

#[allow(unused_mut)]
fn main() {
    // This RefCell is only a minor layer of safety in a wildly unsafe setup
    let mut state_cell = RefCell::new(State::new());
    macro_rules! state {
        () => {&mut *state_cell.borrow_mut()}
    }

    // Initialize the GUI
    Application::create_and_exit(|_app| unsafe {
        let mut state = state_cell.borrow_mut(); // because we need to drop it

        let mut window = widgets::main_window::MainWindow::new();
        state.widgets.window = window.as_mut_ptr();

        // object tree
        let mut tree_widget = widgets::tree_widget::TreeWidget::new();
        state.widgets.tree = tree_widget.as_mut_ptr();
        tree_widget.set_column_count(1);
        tree_widget.set_header_hidden(true);

        // map tabs
        let mut map_tabs = qt::widgets::tab_bar::TabBar::new();
        state.widgets.map_tabs = map_tabs.as_mut_ptr();
        map_tabs.set_tabs_closable(true);
        map_tabs.set_expanding(false);
        map_tabs.set_document_mode(true);
        let tab_close_slot = qt::core::slots::SlotCInt::new(|idx| {
            let state = state!();
            state.close_map(idx as usize);
        });
        map_tabs.signals().tab_close_requested().connect(&tab_close_slot);
        let tab_select_slot = qt::core::slots::SlotCInt::new(|idx| {
            // might be called by adding a new tab, just do nothing
            if let Ok(mut state) = state_cell.try_borrow_mut() {
                state.current_map = idx as usize;
                state.update_current_map();
            }
        });
        map_tabs.signals().current_changed().connect(&tab_select_slot);

        // minimap
        let mut minimap_widget = qt::glium_widget::create(map_renderer::GliumTest);
        minimap_widget.set_minimum_size((256, 256));
        minimap_widget.set_maximum_size((256, 256));

        // tools
        let mut tools = widgets::label::Label::new(qstr!("Tools Go Here"));

        // instances
        let mut list_view = widgets::list_view::ListView::new();

        // map
        let mut map_widget = qt::glium_widget::create(map_renderer::GliumTest);

        // the layouts
        let mut tools_layout = widgets::v_box_layout::VBoxLayout::new();
        tools_layout.add_widget(qt_own!(minimap_widget));
        tools_layout.add_widget(qt_own!(tools));
        tools_layout.add_widget(qt_own!(list_view));

        let mut h_layout = widgets::h_box_layout::HBoxLayout::new();
        h_layout.set_spacing(5);
        h_layout.add_layout(qt_own!(tools_layout));
        h_layout.add_widget((qt_own!(map_widget), 1));
        h_layout.set_contents_margins((0, 0, 0, 0));

        let mut tabbed_layout = widgets::v_box_layout::VBoxLayout::new();
        tabbed_layout.set_spacing(0);
        tabbed_layout.add_widget(qt_own!(map_tabs));
        tabbed_layout.add_layout((qt_own!(h_layout), 1));
        tabbed_layout.set_contents_margins((0, 0, 0, 0));

        let mut h_layout_widget = widgets::widget::Widget::new();
        h_layout_widget.set_layout(qt_own!(tabbed_layout));

        // root splitter
        let mut splitter = widgets::splitter::Splitter::new(());
        splitter.set_children_collapsible(false);
        splitter.add_widget(qt_own!(tree_widget));
        splitter.add_widget(qt_own!(h_layout_widget));
        splitter.set_stretch_factor(0, 0);
        splitter.set_stretch_factor(1, 1);

        // menus
        let mut menu_bar = widgets::menu_bar::MenuBar::new();
        // file menu
        let mut menu_file = &mut *menu_bar.add_menu(qstr!("File"));
        state.widgets.menu_file = menu_file;
        action!(menu_file, "New", (key = ^CTRL KeyN), (tip = "Create a new map."), (disabled), {
            state!().new_map();
        });
        action!(menu_file, "Open", (key = ^CTRL KeyO), (disabled), (tip = "Open a map."), {
            let state = state!();
            let file = FileDialog::get_open_file_name_unsafe((
                static_cast_mut(state.widgets.window()),
                qstr!("Open Map"),
                qstr!(match state.env.as_ref() {
                    Some(env) => env.root.display().to_string(),
                    None => return,  // no environment, shouldn't be opening maps
                }),
                qstr!("Maps (*.dmm)"),
            )).to_std_string();
            if !file.is_empty() {
                state.load_map(PathBuf::from(file));
            }
        });
        action!(menu_file, "Close", (key = ^CTRL KeyW), (disabled), (tip = "Close the current map."), {
            let state = state!();
            let map = state.current_map;
            state.close_map(map);
        });
        menu_file.add_separator();
        action!(menu_file, "Open Environment", (tip = "Load a DME file."), {
            let state = state!();
            let file = FileDialog::get_open_file_name_unsafe((
                static_cast_mut(state.widgets.window()),
                qstr!("Open Environment"),
                qstr!("."),
                qstr!("Environments (*.dme)"),
            )).to_std_string();
            if !file.is_empty() {
                state.load_env(PathBuf::from(file));
            }
        });
        state.widgets.menu_recent = menu_file.add_menu(qstr!("Recent Environments"));
        menu_file.add_separator();
        action!(menu_file, "Exit", (key = ^ALT KeyF4), (slot = window.slots().close()));

        // help menu
        let mut menu_help = &mut *menu_bar.add_menu(qstr!("Help"));
        action!(menu_help, "User Guide", (key = KeyF1));
        action!(menu_help, "About", {
            use qt::widgets::message_box::*;
            let mut mbox = MessageBox::new((
                Icon::Information,
                qstr!("About SpacemanDMM"),
                qstr!(concat!(
                    "SpacemanDMM v", env!("CARGO_PKG_VERSION"), "\n",
                    "by SpaceManiac, for /tg/station13",
                )),
                Flags::from_enum(StandardButton::Ok),
            ));
            {
                let widget: &mut Widget = mbox.static_cast_mut();
                widget.set_attribute(qt::core::qt::WidgetAttribute::DeleteOnClose);
            }
            mbox.show();
            mbox.into_raw();
        });

        // status bar
        let mut status_bar = widgets::status_bar::StatusBar::new();
        state.widgets.status_bar = status_bar.as_mut_ptr();

        // build main window
        window.set_window_title(qstr!("SpacemanDMM"));
        window.resize((1400, 768));

        window.set_menu_bar(qt_own!(menu_bar));
        window.set_status_bar(qt_own!(status_bar));
        window.set_central_widget(qt_own!(splitter));
        window.show();

        // parse command-line arguments:
        // - use the specified DME, or autodetect one from the first DMM
        // - preload all maps specified belonging to that DME
        let mut preload_maps = Vec::new();
        for arg in std::env::args_os() {
            let path = PathBuf::from(arg);

            if path.extension() == Some("dme".as_ref()) {
                if state.env.is_some() {
                    // only one DME may be specified
                    continue;
                }
                state.load_env(path);
            } else if path.extension() == Some("dmm".as_ref()) {
                // determine the corresponding DME
                let detected_env = match detect_environment(&path) {
                    Some(env) => env,
                    None => continue,
                };

                if let Some(env) = state.env.as_ref() {
                    if !is_same_file(&env.dme, detected_env).unwrap_or(false) {
                        preload_maps.push(path);
                    }
                    continue;
                }

                state.load_env(detected_env);
                preload_maps.push(path);
            } else {
                continue;
            }
        }

        for map in preload_maps {
            state.load_map(map);
        }

        // cede control
        state.finish_init();
        drop(state);  // release the RefCell
        Application::exec()
    })
}

// ----------------------------------------------------------------------------
// Main window state handling

impl State {
    fn new() -> State {
        State {
            widgets: Default::default(),
            config: Config::load(),
            env: None,
            maps: Vec::new(),
            current_map: 0,
        }
    }

    unsafe fn finish_init(&mut self) {
        if self.env.is_none() {
            if let Some(env_path) = self.config.recent.first().cloned() {
                self.load_env(env_path);
            }
        }
        self.update_recent();
    }

    unsafe fn update_recent(&mut self) {
        let menu = self.widgets.menu_recent();
        menu.clear();
        if self.config.recent.is_empty() {
            action!(menu, "No recent environments", (disabled));
            return;
        }

        for (i, path) in self.config.recent.iter().enumerate() {
            action!(menu, path.display().to_string());
        }
    }

    unsafe fn load_env(&mut self, path: PathBuf) {
        let self_ptr: *mut Self = self;

        // show the messages and lock the interface
        println!("Environment: {}", path.display());
        self.widgets.status_bar().show_message(qstr!("Loading environment, please wait..."));
        self.widgets.tree().set_disabled(true);
        {
            // disable the actions except for exit
            let actions = self.widgets.menu_file().actions();
            for i in 0..(actions.count() - 1) {
                (**actions.at(i)).set_disabled(true);
            }
        }

        let path2 = path.clone();
        qt::future::spawn(move || -> Result<ObjectTree, String> {
            let mut preprocessor;
            match dm::preprocessor::Preprocessor::new(path2.clone()) {
                Err(_) => {
                    return Err(format!("Could not open for reading:\n{}", path2.display()));
                },
                Ok(pp) => preprocessor = pp,
            }

            dm::parser::parse(dm::indents::IndentProcessor::new(&mut preprocessor)).map_err(|e| {
                let mut message = format!("\
                    Could not parse the environment:\n\
                    {}\n\n\
                    This may be caused by incorrect or unusual code, but is typically a parser bug. \
                    Change the code to use a more common form, or report the parsing problem.\n\
                ", path2.display());
                let mut message_buf = Vec::new();
                let _ = dm::pretty_print_error(&mut message_buf, &preprocessor, &e);
                message.push_str(&String::from_utf8_lossy(&message_buf[..]));
                message
            })
        }, move |result| {
            let this = &mut *self_ptr;
            match result {
                Ok(objtree) => {
                    // fill the object tree
                    {
                        let widget = this.widgets.tree();
                        widget.clear();
                        let root = objtree.root();
                        for &root_child in ["area", "turf", "obj", "mob"].iter() {
                            let ty = root.child(root_child, &objtree).expect("builtins missing");

                            let mut root_item = TreeWidgetItem::new(());
                            root_item.set_text(0, qstr!(&ty.name));
                            add_children(&mut root_item, ty, &objtree);
                            widget.add_top_level_item(qt_own!(root_item));
                        }
                    }
                    this.config.make_recent(&path);
                    this.update_recent();
                    this.config.save();

                    this.env = Some(Environment {
                        root: path.parent().unwrap().to_owned(),
                        dme: path,
                        objtree: objtree,
                    });
                }
                Err(message) => {
                    show_error(this.widgets.window(), &message);
                }
            }

            // un-disable everything
            this.widgets.status_bar().clear_message();
            this.widgets.tree().set_disabled(false);
            let actions = this.widgets.menu_file().actions();
            for i in 0..actions.count() {
                (**actions.at(i)).set_disabled(false);
            }
        });
    }

    unsafe fn new_map(&mut self) {
        // self.widgets.map_tabs().add_tab(qstr!("New Map"));
    }

    unsafe fn load_map(&mut self, path: PathBuf) {
        println!("Map: {}", path.display());

        // Verify that we're in the right environment
        let detected = detect_environment(&path);
        match (detected, self.env.as_ref()) {
            (Some(detected), Some(env)) => if !is_same_file(detected, &env.dme).unwrap_or(false) {
                return show_error(self.widgets.window(), "The map belongs to a different environment.");
            },
            (None, Some(_)) => if !ask_warning(self.widgets.window(), "The map has no environment.\nWould you like to load it anyways?") {
                return
            }
            _ => return,  // Shouldn't happen
        }

        let map = match dmm_tools::dmm::Map::from_file(&path) {
            Err(e) => {
                let message = format!("Could not load the map:\n{}\n\n{}", path.display(), e.description());
                return show_error(self.widgets.window(), &message);
            }
            Ok(map) => map,
        };

        match path.file_name() {
            None => return show_error(self.widgets.window(), "Weird: no filename?"),
            Some(file_name) => { self.widgets.map_tabs().add_tab(qstr!(file_name.to_string_lossy())); }
        }

        self.current_map = self.maps.len();
        self.widgets.map_tabs().set_current_index(self.current_map as i32);
        self.maps.push(Map {
            path: path,
            dmm: map,
        });
        self.update_current_map();
    }

    unsafe fn close_map(&mut self, index: usize) {
        if index >= self.maps.len() { return }
        self.maps.remove(index);
        self.widgets.map_tabs().remove_tab(index as i32);
        self.update_current_map();
    }

    unsafe fn update_current_map(&mut self) {
        if self.maps.is_empty() {
            self.current_map = 0;
            // clear the displays
            return
        }

        if self.current_map >= self.maps.len() {
            self.current_map = self.maps.len() - 1;
        }

        // ...
    }
}

// ----------------------------------------------------------------------------
// Helpers

unsafe fn add_children(parent: &mut TreeWidgetItem, ty: TypeRef, tree: &ObjectTree) {
    let mut children = ty.children(tree);
    children.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    for each in children {
        let mut child = TreeWidgetItem::new(());
        child.set_text(0, qstr!(&each.name));
        add_children(&mut child, each, tree);
        parent.add_child(qt_own!(child));
    }
}

fn show_error(window: &mut Widget, message: &str) {
    use qt::widgets::message_box::*;
    unsafe {
        MessageBox::critical((
            window as *mut Widget,
            qstr!("Error"),
            qstr!(message),
            Flags::from_enum(StandardButton::Ok),
        ));
    }
}

fn ask_warning(window: &mut Widget, message: &str) -> bool {
    use qt::widgets::message_box::*;
    unsafe {
        MessageBox::warning((
            window as *mut Widget,
            qstr!("Warning"),
            qstr!(message),
            Flags::from_enum(StandardButton::Ok) | Flags::from_enum(StandardButton::Cancel),
        )) == StandardButton::Ok
    }
}

fn detect_environment(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent();
    while let Some(dir) = current {
        let read_dir = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return None,
        };
        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return None,
            };
            let path = entry.path();
            if path.extension() == Some("dme".as_ref()) {
                return Some(path);
            }
        }
        current = dir.parent();
    }
    None
}
