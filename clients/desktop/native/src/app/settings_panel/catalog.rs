//! One set of native controls for both settings pages and palette results.
//! Keep search metadata beside the control, not in a second preference registry.

use std::rc::Rc;

use gpui::{App, IntoElement, SharedString, Window};
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

pub(crate) struct Group {
    inner: kit::SettingGroup,
    title: Option<SharedString>,
    items: Vec<Item>,
}

impl Group {
    pub fn new() -> Self {
        Self {
            inner: kit::SettingGroup::new(),
            title: None,
            items: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        let title = title.into();
        self.inner = self.inner.title(title.clone());
        self.title = Some(title);
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.inner = self.inner.description(description);
        self
    }

    pub fn item(mut self, item: Item) -> Self {
        self.inner = self.inner.item(item.control.clone());
        self.items.push(item);
        self
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
}

impl Content {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            inner: kit::SettingPage::new(title),
            entries: Vec::new(),
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
                .into_iter()
                .map(|item| (group.title.clone(), item)),
        );
        self.inner = self.inner.group(group.inner);
        self
    }
}
