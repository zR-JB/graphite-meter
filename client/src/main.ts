import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";

// Chromium lights a clicked control on any later key, even Shift or a screenshot chord; only a real key press does here.
const root = document.documentElement;
const MODIFIER =
  /^(Shift|Control|Alt|AltGraph|Meta|OS|CapsLock|Fn|PrintScreen)$/;
addEventListener("pointerdown", () => (root.dataset.pointer = ""), true);
addEventListener(
  "keydown",
  (event: KeyboardEvent) => {
    const chord = event.ctrlKey || event.metaKey || event.altKey;
    if (!chord && !MODIFIER.test(event.key)) delete root.dataset.pointer;
  },
  true,
);

mount(App, {
  target: document.getElementById("app")!,
});
