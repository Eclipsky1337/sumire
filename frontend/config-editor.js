import { indentWithTab } from "@codemirror/commands";
import { json } from "@codemirror/lang-json";
import { yaml } from "@codemirror/lang-yaml";
import { syntaxTree } from "@codemirror/language";
import { lintGutter, linter } from "@codemirror/lint";
import { Compartment, StateEffect, StateField, Transaction } from "@codemirror/state";
import { oneDark } from "@codemirror/theme-one-dark";
import { Decoration, EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";

const setPendingChanges = StateEffect.define();

const pendingChangesField = StateField.define({
  create() {
    return { changes: [], managed: false, decorations: Decoration.none };
  },
  update(value, transaction) {
    let changes = value.changes;
    let managed = value.managed;
    let metadataChanged = false;
    for (const effect of transaction.effects) {
      if (effect.is(setPendingChanges)) {
        changes = effect.value.changes;
        managed = effect.value.managed;
        metadataChanged = true;
      }
    }
    if (!transaction.docChanged && !metadataChanged) return value;
    return {
      changes,
      managed,
      decorations: buildPendingDecorations(transaction.state.doc, changes, managed),
    };
  },
  provide: field => EditorView.decorations.from(field, value => value.decorations),
});

function buildPendingDecorations(document, changes, managed) {
  if (!managed || changes.length === 0) return Decoration.none;
  const paths = yamlPathsByLine(document.toString());
  const decorations = [];
  for (let index = 0; index < paths.length; index++) {
    const path = paths[index];
    if (!path) continue;
    const change = changes.find(item => path === item.path || path.startsWith(`${item.path}.`));
    if (!change) continue;
    decorations.push(Decoration.line({ class: `config-editor-line-${change.requires}` }).range(document.line(index + 1).from));
  }
  return Decoration.set(decorations, true);
}

function yamlPathsByLine(text) {
  const stack = [];
  return text.split("\n").map(line => {
    const match = /^(\s*)(?:"([^"]+)"|'([^']+)'|([A-Za-z0-9_.-]+))\s*:(?:\s|$)/.exec(line);
    if (!match) return "";
    const indent = match[1].replaceAll("\t", "  ").length;
    const key = match[2] || match[3] || match[4];
    while (stack.length && stack[stack.length - 1].indent >= indent) stack.pop();
    const path = [...stack.map(item => item.key), key].join(".");
    stack.push({ indent, key });
    return path;
  });
}

function syntaxDiagnostics(view) {
  const diagnostics = [];
  syntaxTree(view.state).iterate({
    enter(node) {
      if (!node.type.isError) return;
      diagnostics.push({
        from: node.from,
        to: Math.min(view.state.doc.length, Math.max(node.from + 1, node.to)),
        severity: "error",
        message: "配置语法错误",
      });
    },
  });
  return diagnostics;
}

export function createConfigEditor(parent, options = {}) {
  const language = new Compartment();
  const cspNonce = document.querySelector('meta[name="csp-nonce"]')?.content || "";
  const view = new EditorView({
    parent,
    extensions: [
      basicSetup,
      oneDark,
      language.of(yaml()),
      pendingChangesField,
      linter(syntaxDiagnostics, { delay: 250 }),
      lintGutter(),
      EditorView.cspNonce.of(cspNonce),
      EditorView.contentAttributes.of({ "aria-label": "配置编辑器" }),
      keymap.of([
        indentWithTab,
        {
          key: "Mod-s",
          preventDefault: true,
          run() {
            options.onSave?.();
            return true;
          },
        },
      ]),
    ],
  });

  return {
    getValue() {
      return view.state.doc.toString();
    },
    setValue(value) {
      const text = String(value ?? "");
      if (text === view.state.doc.toString()) return;
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: text },
        annotations: Transaction.addToHistory.of(false),
      });
    },
    setLanguage(mode) {
      view.dispatch({ effects: language.reconfigure(mode === "json" ? json() : yaml()) });
    },
    setPendingChanges(changes, managed) {
      view.dispatch({ effects: setPendingChanges.of({ changes: Array.isArray(changes) ? changes : [], managed: Boolean(managed) }) });
    },
    focus() {
      view.focus();
    },
  };
}
