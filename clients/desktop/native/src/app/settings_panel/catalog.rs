//! One set of native controls for both settings pages and palette results.
//! Keep search metadata beside the control, not in a second preference registry.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, IntoElement, SharedString, Window, px};
use gpui_kit::component::IconName;
use gpui_kit::component::setting::{self as kit, RenderOptions, SettingField};

use super::Page;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub page: Page,
    pub group: Option<SharedString>,
    pub title: SharedString,
}

impl Key {
    pub fn selector(&self) -> String {
        format!(
            "palette-setting:{:?}:{}:{}",
            self.page,
            self.group.as_deref().unwrap_or(""),
            self.title
        )
    }
}

type Activate = Rc<dyn Fn(&mut App)>;
type SwitchValue = Rc<dyn Fn(&App) -> bool>;

#[derive(Clone)]
pub(crate) struct Item {
    pub control: kit::SettingItem,
    pub title: SharedString,
    pub keywords: Vec<SharedString>,
    pub description: String,
    pub actionable: bool,
    activate: Option<Activate>,
    value: Option<SwitchValue>,
    wide: bool,
}

impl Item {
    pub fn new(title: impl Into<SharedString>, field: impl kit::AnySettingField + 'static) -> Self {
        let title = title.into();
        Self {
            control: kit::SettingItem::new(title.clone(), field),
            title,
            keywords: Vec::new(),
            description: String::new(),
            actionable: true,
            activate: None,
            value: None,
            wide: false,
        }
    }

    pub fn render<E: IntoElement>(
        title: impl Into<SharedString>,
        render: impl Fn(&RenderOptions, &mut Window, &mut App) -> E + 'static,
    ) -> Self {
        let title = title.into();
        Self {
            control: kit::SettingItem::render(render),
            title,
            keywords: Vec::new(),
            description: String::new(),
            actionable: true,
            activate: None,
            value: None,
            wide: false,
        }
    }

    /// Enter on a switch reads its live value, then uses the very same writer
    /// as a click on the native control (including persistence and side effects).
    pub fn switch(
        title: impl Into<SharedString>,
        read: impl Fn(&App) -> bool + 'static,
        write: impl Fn(bool, &mut App) + 'static,
        default: Option<bool>,
    ) -> Self {
        let read = Rc::new(read);
        let write = Rc::new(write);
        let (reader, writer) = (read.clone(), write.clone());
        let mut field =
            SettingField::switch(move |cx| reader(cx), move |value, cx| writer(value, cx));
        if let Some(default) = default {
            field = field.default_value(default);
        }
        let reader = read.clone();
        let mut item = Self::new(title, field);
        item.activate = Some(Rc::new(move |cx| write(!reader(cx), cx)));
        item.value = Some(read);
        item
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self.control = self.control.description(self.description.clone());
        self
    }

    /// Wide pickers and tracks move below their labels in a narrow settings pane.
    pub fn wide(mut self) -> Self {
        self.wide = true;
        self
    }

    /// Pages and exact palette editors share the same narrow-control layout.
    pub fn responsive_control(&self, stacked: bool) -> kit::SettingItem {
        let control = self.control.clone();
        if stacked && self.wide {
            control.layout(gpui::Axis::Vertical)
        } else {
            control
        }
    }

    pub fn keywords<I, S>(mut self, keywords: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self.control = self.control.keywords(self.keywords.clone());
        self
    }

    /// Supporting facts and shortcut legends remain on pages, not as dead-end results.
    pub fn information(mut self) -> Self {
        self.actionable = false;
        self
    }

    pub fn value(&self, cx: &App) -> Option<bool> {
        self.value.as_ref().map(|read| read(cx))
    }

    pub fn is_switch(&self) -> bool {
        self.activate.is_some()
    }

    pub fn activate(&self, cx: &mut App) -> bool {
        let Some(activate) = &self.activate else {
            return false;
        };
        activate(cx);
        true
    }
}

#[derive(Clone)]
pub(crate) struct Group {
    title: Option<SharedString>,
    description: Option<SharedString>,
    items: Vec<Item>,
}

/// Pages inset their groups this far past Kit's list padding, so every
/// heading, label and control shares the page header's text column.
pub(crate) const GUTTER: f32 = 20.;
const KIT_LIST_PADDING: f32 = 16.;

impl Group {
    pub fn new() -> Self {
        Self {
            title: None,
            description: None,
            items: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn item(mut self, item: Item) -> Self {
        self.items.push(item);
        self
    }

    fn filtered(&self, page: Page, query: &str, stacked: bool) -> kit::SettingGroup {
        let group = kit::SettingGroup::new().px(px(GUTTER - KIT_LIST_PADDING));
        let group = match &self.title {
            Some(title) => group.title(title.clone()),
            None => group,
        };
        let group = match &self.description {
            Some(description) => group.description(description.clone()),
            None => group,
        };
        group.items(
            self.items
                .iter()
                .filter(|item| self.matches(item, page, query))
                .map(|item| item.responsive_control(stacked)),
        )
    }

    fn matches(&self, item: &Item, page: Page, query: &str) -> bool {
        let text = format!(
            "{} {} {} {} {} {}",
            page.title(),
            page.sidebar_title(),
            self.title.as_deref().unwrap_or(""),
            item.title,
            item.description,
            item.keywords
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
                .join(" ")
        )
        .to_lowercase();
        query.split_whitespace().all(|word| text.contains(word))
    }

    pub fn items(mut self, items: impl IntoIterator<Item = Item>) -> Self {
        for item in items {
            self = self.item(item);
        }
        self
    }
}

pub(crate) struct Content {
    pub inner: kit::SettingPage,
    pub entries: Vec<(Option<SharedString>, Item)>,
    groups: Vec<Group>,
}

impl Content {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            inner: kit::SettingPage::new(title),
            entries: Vec::new(),
            groups: Vec::new(),
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.inner = self.inner.icon(icon);
        self
    }

    pub fn group(mut self, group: Group) -> Self {
        self.entries.extend(
            group
                .items
                .iter()
                .cloned()
                .map(|item| (group.title.clone(), item)),
        );
        self.groups.push(group);
        self
    }

    pub fn matches(&self, page: Page, query: &str) -> bool {
        self.groups.iter().any(|group| {
            group
                .items
                .iter()
                .any(|item| group.matches(item, page, query))
        })
    }

    pub fn filtered(self, page: Page, query: &str, stacked: bool) -> kit::SettingPage {
        self.inner.groups(
            self.groups
                .iter()
                .map(|group| group.filtered(page, query, stacked)),
        )
    }
}
