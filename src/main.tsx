import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@excalidraw/excalidraw/index.css";
import "./styles.css";
import { showWindowWhenReady } from "./platform";
import { App } from "./App";
import { UiHost } from "./ui";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
    <UiHost />
  </StrictMode>,
);

showWindowWhenReady();
