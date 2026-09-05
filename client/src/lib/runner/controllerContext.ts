import { createContext } from "svelte";
import type { ApplicationController } from "./engine.svelte";

export const [getApplicationController, setApplicationController] =
  createContext<ApplicationController>();
