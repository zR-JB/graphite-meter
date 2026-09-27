import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";

// Chromium turns a clicked control's focus visible on any later key, even Shift; only navigation keys earn a ring.
const root = document.documentElement;
const NAVIGATION = /^(Tab|Arrow\w+|Home|End|Page\w+)$/;
addEventListener("pointerdown", () => (root.dataset.pointer = ""), true);
addEventListener(
  "keydown",
  (event: KeyboardEvent) => {
    if (NAVIGATION.test(event.key)) delete root.dataset.pointer;
  },
  true,
);

mount(App, {
  target: document.getElementById("app")!,
});
