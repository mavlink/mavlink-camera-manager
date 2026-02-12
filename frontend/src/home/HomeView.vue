<template>
  <div style="display: flex; column-gap: 1em">
    <div style="display: flex; column-gap: 1em">
      <div v-for="item in content" :key="item.source">
        <div>
          <h3>Camera: {{ item.name }}</h3>
        </div>
        <div>
          <p>Device: {{ item.source }}</p>
        </div>
        <h4>Configure Stream:</h4>
        <div>
          <StreamForm
            :device="item"
            :streams="streams"
            @onconfigure="(value: any) => configureStream(value)"
          />
        </div>
        <h4>Controls:</h4>
        <div>
          <button type="button" @click="resetControls(item.source)">
            Reset controls
          </button>
        </div>
        <div v-for="control in item.controls" :key="control.id">
          <h5>Name: {{ control.name }}</h5>
          <div v-if="control.configuration.Slider">
            <V4lSlider
              :slider="control.configuration.Slider"
              :name="control.id.toString()"
              @onchange="
                (value: any) => setControl(item.source, control.id, value)
              "
            />
          </div>

          <div v-if="control.configuration.Bool">
            <input
              type="checkbox"
              :checked="control.configuration.Bool.value == 1"
              @change="
                (event: Event) =>
                  setControl(
                    item.source,
                    control.id,
                    (event.target as HTMLInputElement).checked ? 1 : 0
                  )
              "
            />
            <label>On</label>
          </div>

          <div v-if="control.configuration.Menu">
            <select
              @change="
                (event: Event) =>
                  setControl(
                    item.source,
                    control.id,
                    Number((event.target as HTMLSelectElement).value)
                  )
              "
            >
              <option
                v-for="option in control.configuration.Menu.options"
                :key="option.value"
                :value="option.value"
                :selected="option.value == control.configuration.Menu.value"
              >
                {{ option.name }}
              </option>
            </select>
          </div>
        </div>
      </div>
      <div>
        <h3>Streams</h3>
        <button type="button" @click="openWebsiteInTab('/webrtc')">
          WebRTC website
        </button>
        <div v-for="stream in streams" :key="stream.id">
          <div>
            <h3>Name: {{ stream.video_and_stream.name }}</h3>
          </div>
          <div>
            <p>
              Video: {{ getVideoDescription(stream.video_and_stream) }}
            </p>
          </div>
          <div>
            <button
              type="button"
              @click="deleteStream(stream.video_and_stream.name)"
            >
              Delete stream
            </button>
          </div>
          <div>
            <p>Endpoints:</p>
            <div
              style="margin-left: 0.5em"
              v-for="endpoint in stream.video_and_stream.stream_information
                .endpoints"
              :key="endpoint"
            >
              <p>{{ endpoint }}</p>
            </div>
          </div>
          <div>
            <p>Configuration:</p>
            <pre style="margin-left: 0.5em">{{
              JSON.stringify(stream, undefined, 2)
            }}</pre>
          </div>
          <div class="dot-container">
            <h4>Pipeline Visualization:</h4>
            <div
              class="dot-content"
              @click="openDotInNewTab(stream.video_and_stream.name)"
            >
              <div :id="'dot-' + stream.video_and_stream.name"></div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent } from "vue";
import V4lSlider from "./components/V4lSlider.vue";
import StreamForm from "./components/StreamForm.vue";

declare class Viz {
  renderString(src: string): Promise<string>;
}

function loadScript(src: string): Promise<void> {
  return new Promise((resolve, reject) => {
    if (document.querySelector(`script[src="${src}"]`)) {
      resolve();
      return;
    }
    const script = document.createElement("script");
    script.src = src;
    script.onload = () => resolve();
    script.onerror = reject;
    document.head.appendChild(script);
  });
}

export default defineComponent({
  name: "HomeApp",
  components: {
    V4lSlider,
    StreamForm,
  },
  async mounted() {
    await loadScript("https://unpkg.com/viz.js@2.1.2/viz.js");
    await loadScript("https://unpkg.com/viz.js@2.1.2/full.render.js");
    this.requestData();
    this.connectDotWebSocket();
  },
  methods: {
    connectDotWebSocket() {
      const ws = new WebSocket(`ws://${window.location.host}/dot`);
      ws.onopen = () => {
        console.log("DOT WebSocket connected");
      };
      ws.onmessage = (event: MessageEvent) => {
        try {
          const dots = JSON.parse(event.data);
          dots.forEach((dot: any) => {
            const stream = this.streams.find((s: any) => s.id === dot.id);
            if (stream) {
              console.log(
                "Processing stream:",
                stream.video_and_stream.name
              );

              // Clear existing content before rendering new dots
              const container = document.getElementById(
                `dot-${stream.video_and_stream.name}`
              );
              if (container) {
                container.innerHTML = "";
              }

              if (
                dot.dot &&
                typeof dot.dot === "string" &&
                dot.dot.trim() !== ""
              ) {
                console.log(
                  "Rendering main dot for:",
                  stream.video_and_stream.name
                );
                this.renderDot(stream.video_and_stream.name, dot.dot, 0);
              } else {
                console.log(
                  "Skipping invalid main dot for:",
                  stream.video_and_stream.name
                );
              }

              if (dot.children && Array.isArray(dot.children)) {
                console.log(
                  "Processing children for:",
                  stream.video_and_stream.name
                );
                dot.children.forEach((childDot: string, index: number) => {
                  if (
                    childDot &&
                    typeof childDot === "string" &&
                    childDot.trim() !== ""
                  ) {
                    console.log("Rendering child dot:", index + 1);
                    this.renderDot(
                      stream.video_and_stream.name,
                      childDot,
                      index + 1
                    );
                  } else {
                    console.log("Skipping invalid child dot:", index + 1);
                  }
                });
              }
            }
          });
        } catch (error) {
          console.error("Error processing DOT data:", error);
        }
      };
      ws.onerror = (error: Event) => {
        console.error("DOT WebSocket error:", error);
      };
      ws.onclose = () => {
        console.log("DOT WebSocket closed, reconnecting...");
        setTimeout(() => this.connectDotWebSocket(), 1000);
      };
    },
    async renderDot(streamName: string, dot: string, streamIndex = 0) {
      console.log(
        "Rendering DOT for stream:",
        streamName,
        "index:",
        streamIndex
      );
      const container = document.getElementById(`dot-${streamName}`);
      if (!container) {
        console.error("Container not found for stream:", streamName);
        return;
      }

      // Skip if dot is empty, undefined, or doesn't contain valid DOT syntax
      if (
        !dot ||
        typeof dot !== "string" ||
        dot.trim() === "" ||
        (!dot.includes("digraph") && !dot.includes("subgraph"))
      ) {
        console.log(
          "Skipping invalid dot for stream:",
          streamName,
          "index:",
          streamIndex,
          "content:",
          dot
        );
        return;
      }

      try {
        const viz = new Viz();

        let wrapper = container.querySelector(
          ".dot-wrapper"
        ) as HTMLElement | null;
        if (!wrapper) {
          wrapper = document.createElement("div");
          wrapper.className = "dot-wrapper";
          wrapper.style.display = "flex";
          wrapper.style.flexDirection = "column";
          wrapper.style.gap = "1em";
          container.innerHTML = "";
          container.appendChild(wrapper);
        }

        const dotContainer = document.createElement("div");
        dotContainer.className = `dot-container-${streamIndex}`;

        console.log("Attempting to render DOT");
        const result = await viz.renderString(dot);
        if (!result || result.trim() === "") {
          console.error("Empty result from viz.renderString");
          return;
        }
        dotContainer.innerHTML = result;
        wrapper.appendChild(dotContainer);
      } catch (error) {
        console.error("Failed to render DOT:", error, "Content:", dot);
        const errorContainer = document.createElement("pre");
        errorContainer.textContent = dot;
        const target =
          container.querySelector(".dot-wrapper") || container;
        target.appendChild(errorContainer);
      }
    },
    openWebsiteInTab(url: string) {
      window.open(url, "_blank");
    },
    getVideoDescription(video_and_stream: any): string {
      let response = "";
      switch (video_and_stream.stream_information.configuration.type) {
        case "redirect":
          break;
        default: {
          response +=
            video_and_stream.stream_information.configuration.encode +
            " " +
            video_and_stream.stream_information.configuration.width +
            "x" +
            video_and_stream.stream_information.configuration.height +
            " @ " +
            video_and_stream.stream_information.configuration.frame_interval
              .denominator +
            " / " +
            video_and_stream.stream_information.configuration.frame_interval
              .numerator +
            " FPS";
        }
      }
      response +=
        ", Thermal: " +
        (video_and_stream.stream_information.extended_configuration?.thermal ??
          false);
      response +=
        ", Disable Mavlink: " +
        (video_and_stream.stream_information.extended_configuration
          ?.disable_mavlink ?? false);
      response +=
        ", Disable Zenoh: " +
        (video_and_stream.stream_information.extended_configuration
          ?.disable_zenoh ?? false);
      response +=
        ", Disable Thumbnails: " +
        (video_and_stream.stream_information.extended_configuration
          ?.disable_thumbnails ?? false);
      return response;
    },
    async requestData() {
      const response_content = await fetch("/v4l");
      this.content = await response_content.json();

      const response_streams = await fetch("/streams");
      this.streams = await response_streams.json();
    },
    async setControl(source: string, id: number, value: number) {
      console.log(
        `Configuring: source: ${source}, control_id: ${id}, value: ${value}`
      );
      const settings = {
        method: "POST",
        body: JSON.stringify({
          device: source,
          v4l_id: Number(id),
          value: Number(value),
        }),
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
        },
      };
      const response = await fetch("/v4l", settings);
      this.checkResponse(response);
    },
    async resetControls(source: string) {
      console.log(
        `Resetting: source: ${source} controls to its default values.`
      );
      const settings = {
        method: "POST",
        body: JSON.stringify({ device: source }),
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
        },
      };
      const response = await fetch("/camera/reset_controls", settings);
      this.checkResponse(response);
    },
    async deleteStream(stream_name: string) {
      console.log(`Deleting stream: ${stream_name}`);

      const url = new URL("/delete_stream", window.location.href);
      url.searchParams.set("name", stream_name);
      const response = await fetch(url, { method: "DELETE" });
      await this.checkResponse(response);
      this.requestData();
    },
    async checkResponse(response: Response): Promise<any> {
      if (!response.ok) {
        // To make the alert text more human readable, here we are:
        //   1. removing the external double quotes pair
        //   2. unescaping new lines
        //   3. unescaping double quotes
        const text = await response
          .text()
          .then((text) =>
            text
              .replace(/^"(.+(?="$))"$/, "$1")
              .replaceAll("\\n", "\n")
              .replaceAll('\\"', '"')
          );
        console.warn(`Something went wrong: ${text}`);
        alert(text);
      } else {
        const contentType = response.headers.get("content-type");
        if (contentType && contentType.indexOf("application/json") !== -1) {
          return await response.json();
        }
      }
      return undefined;
    },
    async configureStream(stream: any) {
      const configuration = (() => {
        switch (stream.source) {
          case "Redirect":
            return {
              type: "redirect",
            };
          default:
            return {
              type: "video",
              encode: stream.configuration.encode,
              height: Number(stream.configuration.size.height),
              width: Number(stream.configuration.size.width),
              frame_interval: stream.configuration.interval,
            };
        }
      })();

      const content = {
        name: stream.name,
        source: stream.source,
        stream_information: {
          endpoints: stream.endpoints
            ? stream.endpoints.split(",")
            : ["udp://0.0.0.0:5600"],
          configuration: configuration,
          extended_configuration: {
            thermal: Boolean(stream.extended_configuration.thermal),
            disable_mavlink: Boolean(
              stream.extended_configuration.disable_mavlink
            ),
            disable_zenoh: Boolean(
              stream.extended_configuration.disable_zenoh
            ),
            disable_thumbnails: Boolean(
              stream.extended_configuration.disable_thumbnails
            ),
          },
        },
      };
      console.log(
        `Configuring new stream: ${JSON.stringify(content, null, 2)}`
      );

      const settings = {
        method: "POST",
        body: JSON.stringify(content),
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
        },
      };
      const response = await fetch("/streams", settings);
      await this.checkResponse(response);
      this.requestData();
    },
    openDotInNewTab(streamName: string) {
      const container = document.getElementById(`dot-${streamName}`);
      if (!container) return;

      const wrapper = container.querySelector(".dot-wrapper");
      if (!wrapper) return;

      // Create a new window with all SVGs
      const newWindow = window.open("", "_blank");
      if (!newWindow) return;

      // Create the HTML content
      const html = [
        "<!DOCTYPE html>",
        "<html>",
        "<head>",
        '    <title>Pipeline Visualization - ' + streamName + "</title>",
        "    <style>",
        "        body {",
        "            margin: 0;",
        "            padding: 0;",
        "            background: #1a1a1a;",
        "            color: #fff;",
        "            display: flex;",
        "            flex-direction: column;",
        "            min-height: 100vh;",
        "        }",
        "        .container {",
        "            width: 100%;",
        "            margin: 0;",
        "            padding: 0;",
        "            display: flex;",
        "            flex-direction: column;",
        "        }",
        "        .pipeline-container {",
        "            width: 100%;",
        "            margin: 0;",
        "            padding: 1em;",
        "            display: flex;",
        "            flex-direction: column;",
        "            box-sizing: border-box;",
        "        }",
        "        .pipeline-title {",
        "            margin: 0 0 0.5em 0;",
        "            color: #fff;",
        "            font-size: 1.2em;",
        "        }",
        "        iframe {",
        "            width: 100%;",
        "            height: 500px;",
        "            border: 1px solid #444;",
        "            border-radius: 4px;",
        "            background: #222;",
        "            box-sizing: border-box;",
        "        }",
        "    </style>",
        "</head>",
        "<body>",
        '    <div class="container">',
        '        <div id="pipelines"></div>',
        "    </div>",
        "</body>",
        "</html>",
      ].join("\n");

      newWindow.document.write(html);
      newWindow.document.close();

      // Add all SVGs to the new window
      const pipelinesContainer =
        newWindow.document.getElementById("pipelines");
      const svgs = wrapper.querySelectorAll("svg");

      // Function to create iframe content
      function createIframeContent(svg: SVGElement) {
        return [
          "<!DOCTYPE html>",
          "<html>",
          "<head>",
          "    <style>",
          "        body {",
          "            margin: 0;",
          "            padding: 0;",
          "            background: #222;",
          "            overflow: hidden;",
          "            height: 100vh;",
          "        }",
          "        .svg-container {",
          "            position: relative;",
          "            width: 100%;",
          "            height: 100%;",
          "            overflow: hidden;",
          "        }",
          "        .controls {",
          "            position: absolute;",
          "            top: 1em;",
          "            left: 1em;",
          "            z-index: 1000;",
          "            display: flex;",
          "            gap: 0.5em;",
          "            background: rgba(34, 34, 34, 0.8);",
          "            padding: 0.5em;",
          "            border-radius: 4px;",
          "            backdrop-filter: blur(4px);",
          "        }",
          "        .controls button {",
          "            padding: 0.5em 1em;",
          "            border: 1px solid #444;",
          "            border-radius: 4px;",
          "            background: #333;",
          "            color: #fff;",
          "            cursor: pointer;",
          "        }",
          "        .controls button:hover {",
          "            background: #444;",
          "        }",
          "        svg {",
          "            width: 100%;",
          "            height: auto;",
          "            max-height: 100%;",
          "            transform-origin: center center;",
          "            transition: transform 0.2s ease;",
          "        }",
          "    </style>",
          "    <script>",
          "        let currentScale = 1;",
          "        let isDragging = false;",
          "        let startX, startY;",
          "        let translateX = 0, translateY = 0;",
          "        let lastTranslateX = 0, lastTranslateY = 0;",
          "",
          "        function zoom(factor) {",
          "            currentScale *= factor;",
          "            currentScale = Math.max(0.1, Math.min(50, currentScale));",
          "            updateTransform();",
          "        }",
          "",
          "        function resetZoom() {",
          "            currentScale = 1;",
          "            translateX = 0;",
          "            translateY = 0;",
          "            lastTranslateX = 0;",
          "            lastTranslateY = 0;",
          "            updateTransform();",
          "        }",
          "",
          "        function updateTransform() {",
          '            const svg = document.querySelector("svg");',
          "            svg.style.transform = `translate(${translateX}px, ${translateY}px) scale(${currentScale})`;",
          "        }",
          "",
          '        window.addEventListener("load", () => {',
          '            const container = document.querySelector(".svg-container");',
          '            const svg = document.querySelector("svg");',
          "",
          '            container.addEventListener("mousedown", (e) => {',
          "                isDragging = true;",
          "                startX = e.clientX - translateX;",
          "                startY = e.clientY - translateY;",
          '                container.style.cursor = "grabbing";',
          "            });",
          "",
          '            document.addEventListener("mousemove", (e) => {',
          "                if (!isDragging) return;",
          "                translateX = e.clientX - startX;",
          "                translateY = e.clientY - startY;",
          "                updateTransform();",
          "            });",
          "",
          '            document.addEventListener("mouseup", () => {',
          "                if (!isDragging) return;",
          "                isDragging = false;",
          '                container.style.cursor = "grab";',
          "                lastTranslateX = translateX;",
          "                lastTranslateY = translateY;",
          "            });",
          "",
          '            container.addEventListener("mouseleave", () => {',
          "                if (isDragging) {",
          "                    isDragging = false;",
          '                    container.style.cursor = "grab";',
          "                    lastTranslateX = translateX;",
          "                    lastTranslateY = translateY;",
          "                }",
          "            });",
          "",
          "            // Set initial cursor style",
          '            container.style.cursor = "grab";',
          "        });",
          "    <" + "/script>",
          "</head>",
          "<body>",
          '    <div class="svg-container">',
          '        <div class="controls">',
          '            <button onclick="zoom(1.2)">Zoom In</button>',
          '            <button onclick="zoom(0.8)">Zoom Out</button>',
          '            <button onclick="resetZoom()">Reset</button>',
          "        </div>",
          `        ${svg.outerHTML}`,
          "    </div>",
          "</body>",
          "</html>",
        ].join("\n");
      }

      // Add SVGs to the window
      svgs.forEach((svg, index) => {
        const svgContainer = newWindow.document.createElement("div");
        svgContainer.className = "pipeline-container";

        const title = newWindow.document.createElement("div");
        title.className = "pipeline-title";
        title.textContent =
          index === 0 ? "Main Pipeline" : `Child Pipeline ${index}`;
        svgContainer.appendChild(title);

        const iframe = newWindow.document.createElement("iframe");
        iframe.srcdoc = createIframeContent(svg);
        svgContainer.appendChild(iframe);

        pipelinesContainer?.appendChild(svgContainer);
      });
    },
  },
  data() {
    return {
      content: [] as any[],
      streams: [] as any[],
    };
  },
});
</script>

<style>
.dot-container {
  margin: 1em 0;
  padding: 1em;
  border: 1px solid #444;
  border-radius: 4px;
}
.dot-content {
  max-width: 800px;
  max-height: 300px;
  overflow: auto;
  margin: 0 auto;
  background: #222;
  border-radius: 4px;
  padding: 1em;
  cursor: pointer;
  transition: background-color 0.2s;
  position: relative;
}
.dot-content:hover {
  background: #2a2a2a;
}
.dot-content::after {
  content: "Click to open in new tab";
  position: absolute;
  bottom: 0.5em;
  right: 0.5em;
  font-size: 0.8em;
  color: #666;
  pointer-events: none;
}
.dot-content svg {
  display: block;
  width: 100%;
  height: auto;
  pointer-events: none;
}
.dot-content svg * {
  pointer-events: none;
}

/* Iframe styles */
.pipeline-container iframe {
  width: 100%;
  height: 100%;
  border: none;
  background: #222;
}

.iframe-controls {
  display: flex;
  gap: 0.5em;
  margin-bottom: 1em;
  padding: 1em;
  background: #222;
}

.iframe-controls button {
  padding: 0.5em 1em;
  border: 1px solid #444;
  border-radius: 4px;
  background: #333;
  color: #fff;
  cursor: pointer;
}

.iframe-controls button:hover {
  background: #444;
}

.svg-container {
  display: flex;
  justify-content: center;
  align-items: center;
  overflow: hidden;
  height: calc(100% - 4em);
}

.svg-container svg {
  width: 100%;
  height: auto;
  max-height: 100%;
  transform-origin: center center;
  transition: transform 0.2s ease;
}
</style>
