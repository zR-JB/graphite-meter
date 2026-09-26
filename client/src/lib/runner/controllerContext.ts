import { createContext } from "svelte";
import type { ApplicationController } from "./controller.svelte";

export const [getApplicationController, setApplicationController] =
  createContext<ApplicationController>();
