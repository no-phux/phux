//! Kit controls for actual provider questions; never infer approvals from terminal text.
use super::*;
use phux_protocol::native_agent::{NativeOption, NativeQuestion};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

struct Request {
    binding: Binding,
    question: NativeQuestion,
}

pub(super) struct Draft {
    source: Arc<Request>,
    inputs: BTreeMap<String, Entity<TextareaState>>,
    choices: HashMap<String, Vec<String>>,
    form_options: BTreeMap<String, Vec<NativeOption>>,
    details: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl Draft {
    fn new(
        question: &NativeQuestion,
        binding: &Binding,
        window: &mut Window,
        cx: &mut Context<Desk>,
    ) -> Self {
        let mut draft = Self {
            source: Arc::new(Request {
                binding: binding.clone(),
                question: question.clone(),
            }),
            inputs: BTreeMap::new(),
            choices: HashMap::new(),
            form_options: BTreeMap::new(),
            details: (question.kind == "permission"
                && question
                    .fields
                    .as_object()
                    .is_some_and(|fields| !fields.is_empty()))
            .then(|| {
                format!(
                    "```json\n{}\n```",
                    serde_json::to_string_pretty(&question.fields).unwrap_or_default()
                )
            }),
            _subscriptions: Vec::new(),
        };
        draft.populate_inputs(question, window, cx);
        draft
    }

    fn populate_inputs(
        &mut self,
        question: &NativeQuestion,
        window: &mut Window,
        cx: &mut Context<Desk>,
    ) {
        match question.kind.as_str() {
            "input" | "editor" => self.input(
                "text",
                question
                    .fields
                    .get("initial_value")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                window,
                cx,
            ),
            "ask" => {
                if question
                    .fields
                    .get("allow_custom_input")
                    .and_then(Value::as_bool)
                    != Some(false)
                {
                    for item in &question.questions {
                        self.input(&item.id, "", window, cx);
                    }
                }
            }
            "form" => {
                if let Some(properties) =
                    question.fields.get("properties").and_then(Value::as_object)
                {
                    for (id, schema) in properties {
                        if let Some(offered) = schema_options(schema) {
                            self.form_options.insert(id.clone(), offered);
                        } else {
                            self.input(id, "", window, cx);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn input(&mut self, id: &str, initial: &str, window: &mut Window, cx: &mut Context<Desk>) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(false)
        });
        if !initial.is_empty() {
            input.update(cx, |state, cx| state.set_value(initial, window, cx));
        }
        self._subscriptions
            .push(cx.subscribe_in(&input, window, |_, _, _: &InputEvent, _, cx| cx.notify()));
        self.inputs.insert(id.to_owned(), input);
    }

    fn text(&self, id: &str, cx: &gpui::App) -> String {
        self.inputs
            .get(id)
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }
    fn response(&self, cx: &gpui::App) -> Result<Value, String> {
        match self.source.question.kind.as_str() {
            "input" | "editor" => Ok(json!({"text": self.text("text", cx)})),
            "ask" => ask_response(&self.source.question.questions, &self.choices, |id| {
                self.text(id, cx)
            }),
            "form" => self.form_response(cx),
            _ => Err("Choose an explicit response above.".into()),
        }
    }
    fn form_response(&self, cx: &gpui::App) -> Result<Value, String> {
        let properties = self
            .source
            .question
            .fields
            .get("properties")
            .and_then(Value::as_object)
            .ok_or("Provider form has no supported properties schema. Nothing was submitted.")?;
        let mut values = serde_json::Map::new();
        for (id, schema) in properties {
            if let Some(value) = self.form_field(id, schema, &self.text(id, cx))? {
                values.insert(id.clone(), value);
            }
        }
        Ok(json!({"values": values}))
    }
    fn form_field(&self, id: &str, schema: &Value, raw: &str) -> Result<Option<Value>, String> {
        let selected = self.choices.get(id);
        if raw.is_empty() && selected.is_none() && !field_required(&self.source.question, id) {
            return Ok(None);
        }
        if self.form_options.contains_key(id)
            && selected.is_none_or(Vec::is_empty)
            && schema.get("type").and_then(Value::as_str) != Some("array")
        {
            return Err(format!("Choose an explicit option for ‘{id}’."));
        }
        self.selected_form_value(id, schema, raw)
            .map(Some)
            .map_err(|error| format!("{id}: {error}"))
    }
    fn selected_form_value(&self, id: &str, schema: &Value, raw: &str) -> Result<Value, String> {
        let selected = self.choices.get(id);
        if self.form_options.contains_key(id)
            && schema.get("type").and_then(Value::as_str) == Some("array")
        {
            selected_array(selected.map(Vec::as_slice).unwrap_or(&[]))
        } else {
            form_value(
                schema,
                raw,
                selected
                    .and_then(|values| values.first())
                    .map(String::as_str),
            )
        }
    }
}

fn form_value(schema: &Value, raw: &str, selected: Option<&str>) -> Result<Value, String> {
    if let Some(selected) = selected {
        return serde_json::from_str(selected).map_err(|_| "Invalid selected value".into());
    }
    match schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string")
    {
        "string" => Ok(Value::String(raw.to_owned())),
        "number" | "integer" | "boolean" | "object" | "array" => {
            let value: Value = serde_json::from_str(raw)
                .map_err(|_| "Enter a valid JSON value of the requested type".to_owned())?;
            let valid = form_type_matches(schema, &value);
            if valid {
                Ok(value)
            } else {
                Err("Value does not match the requested type".into())
            }
        }
        _ => Err("This provider field type is not supported. Nothing was submitted.".into()),
    }
}

fn form_type_matches(schema: &Value, value: &Value) -> bool {
    match schema.get("type").and_then(Value::as_str) {
        Some("number") => value.is_number(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("boolean") => value.is_boolean(),
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        _ => false,
    }
}

fn field_required(question: &NativeQuestion, id: &str) -> bool {
    question
        .fields
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(id)))
}

fn schema_options(schema: &Value) -> Option<Vec<NativeOption>> {
    let schema = if schema.get("type").and_then(Value::as_str) == Some("array") {
        schema.get("items")?
    } else {
        schema
    };
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        return variants
            .iter()
            .map(|variant| {
                let value = variant.get("const")?;
                Some(NativeOption {
                    id: value.to_string(),
                    label: variant
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string())
                        }),
                    description: variant
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect();
    }
    let values = if schema.get("type").and_then(Value::as_str) == Some("boolean") {
        vec![Value::Bool(true), Value::Bool(false)]
    } else {
        schema.get("enum").and_then(Value::as_array)?.clone()
    };
    Some(
        values
            .into_iter()
            .map(|value| NativeOption {
                id: value.to_string(),
                label: value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
                description: None,
            })
            .collect(),
    )
}

fn selected_array(selected: &[String]) -> Result<Value, String> {
    selected
        .iter()
        .map(|value| {
            serde_json::from_str::<Value>(value)
                .map_err(|_| "Invalid selected form option".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn ask_response(
    questions: &[phux_protocol::native_agent::NativeQuestionItem],
    choices: &HashMap<String, Vec<String>>,
    text: impl Fn(&str) -> String,
) -> Result<Value, String> {
    let mut answers = Vec::with_capacity(questions.len());
    for item in questions {
        let selected = choices.get(&item.id).cloned().unwrap_or_default();
        let custom = text(&item.id);
        if selected.is_empty() && custom.trim().is_empty() {
            return Err(format!("Answer ‘{}’ before submitting.", item.question));
        }
        if (!item.multi && selected.len() > 1)
            || selected
                .iter()
                .any(|id| !item.options.iter().any(|option| &option.id == id))
        {
            return Err("The offered options changed. Select a current option.".into());
        }
        let mut answer = json!({"id": item.id, "selected_options": selected});
        if !custom.trim().is_empty() {
            answer["custom_input"] = Value::String(custom);
        }
        answers.push(answer);
    }
    Ok(json!({"answers": answers}))
}

pub(super) fn prepare(
    desk: &mut Desk,
    placement: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) {
    let Some(face) = desk.conversation_faces.get_mut(placement) else {
        return;
    };
    let view = desk
        .link
        .as_ref()
        .and_then(|link| link.connection.conversations.get(&face.binding.session))
        .filter(|conversation| {
            !conversation.closed && conversation.state.status != AgentSessionStatus::Ended
        })
        .and_then(|conversation| conversation.native.as_ref());
    let Some(view) = view else {
        face.questions.clear();
        face.question_status.clear();
        return;
    };
    face.questions
        .retain(|id, _| view.questions.iter().any(|question| &question.id == id));
    face.question_status
        .retain(|id, _| view.questions.iter().any(|question| &question.id == id));
    for question in &view.questions {
        if face
            .questions
            .get(&question.id)
            .is_none_or(|draft| draft.source.question != *question)
        {
            face.questions.insert(
                question.id.clone(),
                Draft::new(question, &face.binding, window, cx),
            );
            face.question_status.remove(&question.id);
        }
    }
}

impl Desk {
    fn schedule_native_answer(
        &self,
        binding: &Binding,
        request: &Request,
        response: Value,
    ) -> Result<u64, String> {
        let question = request.question.id.as_str();
        let identity = self.conversation_write_identity(binding, false)?;
        let view = identity
            .native
            .as_ref()
            .ok_or("This is an attached TUI, not a native ACP session")?;
        if !view.capabilities.questions.is_supported()
            || !view.questions.iter().any(|item| item == &request.question)
        {
            return Err("The provider request closed or changed. Nothing was sent.".into());
        }
        let client = self.client().ok_or("Not connected")?;
        let operation = client.with_control(|control| {
            gate(control, binding, &identity, false)?;
            control
                .native_agent_command(
                    &binding.session,
                    &view.activation,
                    NativeIntent::Answer {
                        question_id: question.to_owned(),
                        response,
                    },
                )
                .ok_or_else(|| "Response not scheduled; your draft is preserved.".to_owned())
        });
        client.wake();
        operation
    }

    fn answer_native(
        &mut self,
        placement: &str,
        request: &Request,
        response: Option<Value>,
        cx: &mut Context<Self>,
    ) {
        let question = request.question.id.as_str();
        let Some(face) = self.conversation_faces.get(placement) else {
            return;
        };
        if face.question_status.contains_key(question) {
            return;
        }
        if face.binding != request.binding {
            return;
        }
        let binding = face.binding.clone();
        let response = response.map(Ok).unwrap_or_else(|| {
            face.questions
                .get(question)
                .ok_or_else(|| "This request has closed.".to_owned())
                .and_then(|draft| draft.response(cx))
        });
        let result =
            response.and_then(|response| self.schedule_native_answer(&binding, request, response));
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            match result {
                Ok(operation) => {
                    face.native_commands
                        .insert(operation, Some(question.to_owned()));
                    face.question_status.insert(
                        question.to_owned(),
                        "Submitting response; waiting for ACP receipt…".into(),
                    );
                    face.error = None;
                }
                Err(error) => face.error = Some(error),
            }
        }
        cx.notify();
    }
}

fn response_button(
    placement: &str,
    request: Arc<Request>,
    id: &str,
    label: String,
    response: Value,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let placement = placement.to_owned();
    Button::new(SharedString::from(format!(
        "native-answer-{placement}-{}-{id}",
        request.question.id
    )))
    .label(label)
    .small()
    .ghost()
    .disabled(disabled)
    .on_click(cx.listener(move |desk, _, _, cx| {
        desk.answer_native(&placement, &request, Some(response.clone()), cx)
    }))
    .into_any_element()
}

fn options(
    placement: &str,
    draft: &Draft,
    item: &str,
    offered: &[NativeOption],
    multi: bool,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> Vec<AnyElement> {
    let selected = draft.choices.get(item).map(Vec::as_slice).unwrap_or(&[]);
    offered
        .iter()
        .map(|option| {
            let placement = placement.to_owned();
            let question = draft.source.question.id.clone();
            let request = draft.source.clone();
            let item = item.to_owned();
            let option_id = option.id.clone();
            let label = match &option.description {
                Some(detail) => format!("{} — {detail}", option.label),
                None => option.label.clone(),
            };
            Button::new(SharedString::from(format!(
                "native-choice-{placement}-{question}-{item}-{}",
                option.id
            )))
            .label(label)
            .small()
            .ghost()
            .toggled(selected.contains(&option.id))
            .disabled(disabled)
            .on_click(cx.listener(move |desk, _, _, cx| {
                if let Some(draft) = desk
                    .conversation_faces
                    .get_mut(&placement)
                    .and_then(|face| face.questions.get_mut(&question))
                {
                    if draft.source.binding != request.binding
                        || draft.source.question != request.question
                    {
                        return;
                    }
                    let values = draft.choices.entry(item.clone()).or_default();
                    if values.contains(&option_id) {
                        values.retain(|value| value != &option_id);
                    } else {
                        if !multi {
                            values.clear();
                        }
                        values.push(option_id.clone());
                    }
                }
                cx.notify();
            }))
            .into_any_element()
        })
        .collect()
}

pub(super) fn focus(
    face: &Face,
    view: Option<&NativeView>,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> bool {
    let Some(view) =
        view.filter(|view| view.connected && view.capabilities.questions.is_supported())
    else {
        return false;
    };
    for question in &view.questions {
        if face.question_status.contains_key(&question.id) {
            continue;
        }
        if let Some(input) = face
            .questions
            .get(&question.id)
            .and_then(|draft| draft.inputs.values().next())
        {
            input.update(cx, |state, cx| state.focus(window, cx));
            return true;
        }
    }
    false
}

pub(super) fn render(
    face: &Face,
    view: Option<&NativeView>,
    ready: bool,
    placement: &str,
    look: &Look,
    cx: &mut Context<Desk>,
) -> Vec<AnyElement> {
    let Some(view) = view else {
        return Vec::new();
    };
    view.questions
        .iter()
        .map(|question| render_question(face, view, ready, question, placement, look, cx))
        .collect()
}

fn render_question(
    face: &Face,
    view: &NativeView,
    ready: bool,
    question: &NativeQuestion,
    placement: &str,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let status = face.question_status.get(&question.id);
    let disabled = !ready
        || !view.connected
        || !view.capabilities.questions.is_supported()
        || status.is_some();
    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .rounded(corner(look, 8.))
        .border_1()
        .border_color(look.border)
        .child(
            div()
                .text_sm()
                .child(format!("{} · {}", question.title, question.kind)),
        )
        .child(
            TextView::markdown(
                SharedString::from(format!("native-question-{}", question.id)),
                question.message.clone(),
            )
            .selectable(true)
            .scrollable(false),
        );
    let group = |panel: gpui::Div| {
        panel
            .id(SharedString::from(format!(
                "native-question-panel-{}",
                question.id
            )))
            .role(gpui::Role::Group)
            .aria_label(question.title.clone())
            .into_any_element()
    };
    let Some(draft) = face.questions.get(&question.id) else {
        return group(panel);
    };
    if let Some(details) = &draft.details {
        panel = panel.child(
            TextView::markdown(
                SharedString::from(format!("native-question-details-{}", question.id)),
                details.clone(),
            )
            .selectable(true)
            .scrollable(false),
        );
    }
    panel = render_controls(panel, question, draft, placement, disabled, cx);
    panel = render_submit(panel, question, draft, placement, disabled, cx);
    panel = panel.child(response_button(
        placement,
        draft.source.clone(),
        "cancel",
        "Cancel request".into(),
        json!({"cancelled":true}),
        disabled,
        cx,
    ));
    if let Some(status) = status {
        panel = panel.child(div().text_xs().child(status.clone()));
    }
    group(panel)
}

fn render_controls(
    panel: gpui::Div,
    question: &NativeQuestion,
    draft: &Draft,
    placement: &str,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    match question.kind.as_str() {
        "confirm" => render_confirm(panel, draft, placement, disabled, cx),
        "select" | "permission" => {
            render_permission(panel, question, draft, placement, disabled, cx)
        }
        "input" | "editor" => render_text(panel, draft, "text", &question.title, disabled),
        "ask" => render_ask(panel, question, draft, placement, disabled, cx),
        "form" => render_form(panel, question, draft, placement, disabled, cx),
        _ => panel.child(
            "Unsupported native request type. No approval or terminal fallback will be sent.",
        ),
    }
}
fn render_confirm(
    mut panel: gpui::Div,
    draft: &Draft,
    placement: &str,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    for (id, label, confirmed) in [("yes", "Confirm", true), ("no", "Decline", false)] {
        panel = panel.child(response_button(
            placement,
            draft.source.clone(),
            id,
            label.into(),
            json!({"confirmed":confirmed}),
            disabled,
            cx,
        ));
    }
    panel
}
fn render_permission(
    mut panel: gpui::Div,
    question: &NativeQuestion,
    draft: &Draft,
    placement: &str,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    for option in &question.options {
        let label = match &option.description {
            Some(description) => format!("{} — {description}", option.label),
            None => option.label.clone(),
        };
        panel = panel.child(response_button(
            placement,
            draft.source.clone(),
            &option.id,
            label,
            json!({"option_id":option.id}),
            disabled,
            cx,
        ));
    }
    panel
}
fn render_text(
    mut panel: gpui::Div,
    draft: &Draft,
    id: &str,
    label: &str,
    disabled: bool,
) -> gpui::Div {
    if let Some(input) = draft.inputs.get(id) {
        panel = panel.child(
            Textarea::new(input)
                .aria_label(label.to_owned())
                .w_full()
                .disabled(disabled),
        );
    }
    panel
}
fn render_ask(
    mut panel: gpui::Div,
    question: &NativeQuestion,
    draft: &Draft,
    placement: &str,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    for item in &question.questions {
        panel = panel
            .child(div().text_sm().child(format!(
                "{} {}{}",
                item.header,
                item.question,
                if item.multi { " (choose multiple)" } else { "" }
            )))
            .children(options(
                placement,
                draft,
                &item.id,
                &item.options,
                item.multi,
                disabled,
                cx,
            ));
        if draft.inputs.contains_key(&item.id) {
            panel = render_text(
                panel.child(div().text_xs().child("Custom response (optional)")),
                draft,
                &item.id,
                &format!("Custom response to {}", item.question),
                disabled,
            );
        }
    }
    panel
}
fn render_form(
    mut panel: gpui::Div,
    question: &NativeQuestion,
    draft: &Draft,
    placement: &str,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let Some(properties) = question.fields.get("properties").and_then(Value::as_object) else {
        return panel.child("Unsupported provider form schema; no response submitted.");
    };
    for (id, schema) in properties {
        let title = schema.get("title").and_then(Value::as_str).unwrap_or(id);
        panel = panel.child(div().text_sm().child(format!(
            "{}{}",
            title,
            if field_required(question, id) {
                " *"
            } else {
                ""
            }
        )));
        if let Some(description) = schema.get("description").and_then(Value::as_str) {
            panel = panel.child(div().text_xs().child(description.to_owned()));
        }
        if let Some(offered) = draft.form_options.get(id) {
            panel = panel.children(options(
                placement,
                draft,
                id,
                offered,
                schema.get("type").and_then(Value::as_str) == Some("array"),
                disabled,
                cx,
            ));
        } else {
            panel = render_text(panel, draft, id, title, disabled);
        }
    }
    panel
}
fn render_submit(
    mut panel: gpui::Div,
    question: &NativeQuestion,
    draft: &Draft,
    placement: &str,
    disabled: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    if matches!(question.kind.as_str(), "input" | "editor" | "ask" | "form") {
        let placement = placement.to_owned();
        let request = draft.source.clone();
        panel =
            panel.child(
                Button::new(SharedString::from(format!(
                    "native-submit-{placement}-{}",
                    request.question.id
                )))
                .label("Submit response")
                .small()
                .primary()
                .disabled(disabled)
                .on_click(cx.listener(move |desk, _, _, cx| {
                    desk.answer_native(&placement, &request, None, cx)
                })),
            );
    }
    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_values_preserve_types_and_refuse_coercion() {
        assert_eq!(
            form_value(&json!({"type":"integer"}), "12", None).unwrap(),
            json!(12)
        );
        assert!(form_value(&json!({"type":"integer"}), "12.5", None).is_err());
        assert!(form_value(&json!({"type":"boolean"}), "yes", None).is_err());
        assert_eq!(
            form_value(&json!({"type":"string"}), "false", None).unwrap(),
            json!("false")
        );
        assert_eq!(
            form_value(&json!({"enum":["allow"]}), "", Some("\"allow\"")).unwrap(),
            json!("allow")
        );
        assert_eq!(
            form_value(&json!({"type":"boolean"}), "", Some("false")).unwrap(),
            json!(false)
        );
    }

    #[test]
    fn multiple_questions_keep_exact_ids_and_never_select_recommendations() {
        use phux_protocol::native_agent::NativeQuestionItem;
        let questions = vec![
            NativeQuestionItem {
                id: "q-a".into(),
                question: "Choose".into(),
                header: "Plan".into(),
                options: vec![NativeOption {
                    id: "native/one".into(),
                    label: "First".into(),
                    description: None,
                }],
                multi: true,
                recommended: Some("native/one".into()),
            },
            NativeQuestionItem {
                id: "q-b".into(),
                question: "Explain".into(),
                header: String::new(),
                options: vec![],
                multi: false,
                recommended: None,
            },
        ];
        assert!(ask_response(&questions, &HashMap::new(), |_| String::new()).is_err());
        let selected = HashMap::from([("q-a".into(), vec!["native/one".into()])]);
        assert_eq!(
            ask_response(&questions, &selected, |id| if id == "q-b" {
                "Custom answer".into()
            } else {
                String::new()
            })
            .unwrap(),
            json!({"answers":[{"id":"q-a","selected_options":["native/one"]},{"id":"q-b","selected_options":[],"custom_input":"Custom answer"}]})
        );
        let stale = HashMap::from([("q-a".into(), vec!["invented-label".into()])]);
        assert!(ask_response(&questions, &stale, |_| "custom".into()).is_err());
    }

    #[test]
    fn provider_form_options_preserve_one_of_values_labels_and_multi_selection() {
        let schema = json!({"type":"array","items":{"oneOf":[
            {"const":"native/a","title":"Readable A","description":"First actual option"},
            {"const":17,"title":"Readable B"}
        ]}});
        let offered = schema_options(&schema).unwrap();
        assert_eq!(offered[0].id, "\"native/a\"");
        assert_eq!(offered[0].label, "Readable A");
        assert_eq!(offered[1].id, "17");
        assert_eq!(
            selected_array(&[offered[0].id.clone(), offered[1].id.clone()]).unwrap(),
            json!(["native/a", 17])
        );
        assert_eq!(selected_array(&[]).unwrap(), json!([]));
        assert!(schema_options(&json!({"oneOf":[{"type":"string"},{"type":"number"}]})).is_none());
    }
}
