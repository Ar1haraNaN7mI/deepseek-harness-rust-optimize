import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { HarnessRoot } from "./HarnessRoot";
import "./styles.css";
import "./ui-effects.css";
import "../../docs/startup-emblem.js";

createRoot(document.getElementById("root")!).render(
  <StrictMode><HarnessRoot /></StrictMode>,
);
