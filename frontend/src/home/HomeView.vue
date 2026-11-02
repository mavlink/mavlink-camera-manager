<template>
  <div style="display: flex; column-gap: 1em">
    <div style="display: flex; column-gap: 1em">
      <div v-for="item in content" :key="item.source">
        <div>
          <h3>Camera: {{ item.name }}</h3>
        </div>
        <div>
          <p>Device: {{ item.source }}</p>
          <p v-if="thumbnailHref(item.source)">
            <a :href="thumbnailHref(item.source)" target="_blank">Snapshot</a>
          </p>
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
        <p
          v-if="hasRestartRequiredControl(item.controls)"
          class="restart-required-legend"
        >
          <abbr class="restart-required-marker" :title="restartRequiredLegend"
            >[R]</abbr
          >
          {{ restartRequiredLegend }}
        </p>
        <details
          v-for="group in controlPluginGroups(item.controls)"
          :key="group.plugin"
          class="plugin-control-group"
        >
          <summary>{{ group.label }}</summary>
          <div
            v-for="control in group.controls"
            :key="control.id"
            class="plugin-control-item"
          >
            <ControlInputs
              :control="control"
              @onchange="
                (value: any) => setControl(item.source, control.id, value)
              "
            />
          </div>
        </details>
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
          <div
            v-if="stream.restart_needed"
            style="
              border: 1px solid #a60;
              background: #fff4e5;
              padding: 0.5em;
              margin: 0.5em 0;
            "
          >
            <p>
              Stream restart required for pending encoder/pipeline changes.
            </p>
          </div>
          <div>
            <button
              type="button"
              @click="restartStream(stream.video_and_stream.name)"
            >
              Restart stream
            </button>
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
            <p v-if="sdpHref(stream)">
              <a :href="sdpHref(stream)" target="_blank">SDP</a>
            </p>
          </div>
          <details v-if="pipelineControls(stream).length" class="collapsible">
            <summary>Pipeline controls</summary>
            <button
              type="button"
              @click="resetPipelineControls(stream.video_and_stream.name)"
            >
              Reset pipeline controls
            </button>
            <p
              v-if="hasRestartRequiredControl(pipelineControls(stream))"
              class="restart-required-legend"
            >
              <abbr
                class="restart-required-marker"
                :title="restartRequiredLegend"
                >[R]</abbr
              >
              {{ restartRequiredLegend }}
            </p>
            <details
              v-for="group in pipelineControlPluginGroups(stream)"
              :key="group.plugin"
              class="pipeline-plugin-group"
            >
              <summary>{{ group.label }}</summary>
              <div
                v-for="control in group.controls"
                :key="control.id"
                class="pipeline-control-item"
              >
                <ControlInputs
                  :control="control"
                  @onchange="
                    (value: any) =>
                      setStreamControl(
                        stream.video_and_stream.name,
                        control.id,
                        value
                      )
                  "
                />
              </div>
            </details>
          </details>
          <details class="collapsible">
            <summary>Configuration</summary>
            <pre style="margin-left: 0.5em">{{
              JSON.stringify(stream, undefined, 2)
            }}</pre>
          </details>
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
import ControlInputs from "./components/ControlInputs.vue";
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
    ControlInputs,
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
    sdpHref(stream: any): string | undefined {
      if (!stream.running) {
        return undefined;
      }
      const endpoints =
        stream.video_and_stream?.stream_information?.endpoints ?? [];
      if (
        !endpoints.some((endpoint: string) =>
          /^udp(265)?:\/\//.test(endpoint)
        )
      ) {
        return undefined;
      }
      const video_source = stream.video_and_stream?.video_source;
      // Redirect pipelines have no sinks, so /sdp always 500s.
      if (video_source?.Redirect) {
        return undefined;
      }
      const gst_source = video_source?.Gst?.source;
      const source =
        video_source?.Local?.device_path ??
        video_source?.Onvif?.source?.Onvif ??
        gst_source?.Local?.device_path ??
        gst_source?.Fake ??
        gst_source?.QR;
      if (!source) {
        return undefined;
      }
      const url = new URL("/sdp", window.location.href);
      url.searchParams.set("source", source);
      return url.toString();
    },
    streamSource(stream: any): string | undefined {
      const video_source = stream.video_and_stream?.video_source;
      // Redirect is a placeholder source shared by every redirect stream.
      if (video_source?.Redirect) {
        return undefined;
      }
      const gst_source = video_source?.Gst?.source;
      return (
        video_source?.Local?.device_path ??
        video_source?.Onvif?.source?.Onvif ??
        gst_source?.Local?.device_path ??
        gst_source?.Fake ??
        gst_source?.QR
      );
    },
    thumbnailHref(source: string): string | undefined {
      const stream = this.streams.find(
        (s: any) => this.streamSource(s) === source
      );
      if (!stream) {
        return undefined;
      }
      if (
        stream.video_and_stream?.stream_information?.extended_configuration
          ?.disable_thumbnails
      ) {
        return undefined;
      }
      const url = new URL("/thumbnail", window.location.href);
      url.searchParams.set("source", source);
      url.searchParams.set("quality", "75");
      url.searchParams.set("target_height", "240");
      return url.toString();
    },
    getVideoDescription(video_and_stream: any): string {
      let response = "";
      switch (video_and_stream.stream_information.configuration.type) {
        case "redirect":
          break;
        default: {
          const configuration = video_and_stream.stream_information.configuration;
          const source_encode = configuration.source_encode ?? configuration.encode;
          response +=
            (source_encode && source_encode !== configuration.encode
              ? source_encode + " -> " + configuration.encode
              : configuration.encode) +
            " " +
            configuration.width +
            "x" +
            configuration.height +
            " @ " +
            configuration.frame_interval.denominator +
            " / " +
            configuration.frame_interval.numerator +
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
      response +=
        ", Disable Lazy: " +
        (video_and_stream.stream_information.extended_configuration
          ?.disable_lazy ?? false);
      response +=
        ", Disable Recording: " +
        (video_and_stream.stream_information.extended_configuration
          ?.disable_recording ?? false);
      return response;
    },
    async requestData() {
      const response_content = await fetch("/v4l");
      this.content = await response_content.json();

      const response_streams = await fetch("/streams");
      const streams = await response_streams.json();
      this.streams = await Promise.all(
        streams.map(async (stream: any) => {
          stream.pipeline_controls = await this.fetchPipelineControls(
            stream.video_and_stream.name
          );
          return stream;
        })
      );
    },
    pipelineControls(stream: any): any[] {
      return (stream.pipeline_controls ?? []).filter(
        (control: any) =>
          control.id >= 50000000 && control.name !== "restart-stream"
      );
    },
    pipelineControlPluginGroups(
      stream: any
    ): Array<{ plugin: string; label: string; controls: any[] }> {
      return this.controlPluginGroups(this.pipelineControls(stream), true);
    },
    controlPluginGroups(
      controls: any[],
      pipeline_order: boolean = false
    ): Array<{ plugin: string; label: string; controls: any[] }> {
      const groups = new Map<string, any[]>();
      for (const control of controls) {
        const plugin = control.plugin_name || "unknown";
        const bucket = groups.get(plugin) ?? [];
        bucket.push(control);
        groups.set(plugin, bucket);
      }
      return [...groups.entries()]
        .sort(([left_plugin, left_controls], [right_plugin, right_controls]) => {
          if (pipeline_order) {
            const left_order = Math.min(
              ...left_controls.map((control: any) =>
                this.pipelineControlElementOrder(control.element)
              )
            );
            const right_order = Math.min(
              ...right_controls.map((control: any) =>
                this.pipelineControlElementOrder(control.element)
              )
            );
            if (left_order !== right_order) {
              return left_order - right_order;
            }
          }
          return left_plugin.localeCompare(right_plugin);
        })
        .map(([plugin, grouped_controls]) => ({
          plugin,
          label: this.pluginLabel(plugin),
          controls: grouped_controls,
        }));
    },
    pluginLabel(plugin: string): string {
      if (plugin === "stream") {
        return "Stream";
      }
      return plugin;
    },
    pipelineControlElementOrder(element: string): number {
      if (element === "decoder" || element.includes("dec")) {
        return 0;
      }
      if (element === "encoder" || element.includes("enc")) {
        return 1;
      }
      if (element === "stream") {
        return 3;
      }
      return 2;
    },
    hasRestartRequiredControl(controls: any[]): boolean {
      return (controls ?? []).some((control: any) => control.requires_restart);
    },
    streamControlsUrl(stream_name: string): string {
      return "/streams/" + encodeURIComponent(stream_name) + "/controls";
    },
    async fetchPipelineControls(stream_name: string): Promise<any[]> {
      const response = await fetch(this.streamControlsUrl(stream_name));
      if (!response.ok) {
        return [];
      }
      return await response.json();
    },
    async setStreamControl(stream_name: string, id: number, value: number) {
      const settings = {
        method: "POST",
        body: JSON.stringify({
          id: Number(id),
          value: Number(value),
        }),
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
        },
      };
      const response = await fetch(this.streamControlsUrl(stream_name), settings);
      await this.checkResponse(response);
      this.requestData();
    },
    async resetPipelineControls(stream_name: string) {
      const response = await fetch(
        this.streamControlsUrl(stream_name) + "/reset",
        { method: "POST" }
      );
      await this.checkResponse(response);
      this.requestData();
    },
    async restartStream(stream_name: string) {
      const url = new URL("/streams/restart", window.location.href);
      url.searchParams.set("name", stream_name);
      const response = await fetch(url, { method: "POST" });
      await this.checkResponse(response);
      this.requestData();
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
          default: {
            const source_encode = stream.configuration.source_encode;
            const sink_encode = stream.configuration.encode;
            const is_raw_source = ["NV12", "YUYV", "RGB"].includes(
              source_encode
            );
            const is_compressed_source = ["MJPG", "H264", "H265"].includes(
              source_encode
            );
            const is_raw_sink = ["NV12", "YUYV", "RGB"].includes(sink_encode);
            const use_manual_transcoding =
              source_encode &&
              sink_encode &&
              source_encode !== sink_encode &&
              (is_raw_source
                ? Boolean(stream.configuration.encoder)
                : is_compressed_source &&
                  stream.configuration.transcoding_mode === "manual" &&
                  (is_raw_sink || Boolean(stream.configuration.encoder)));
            const use_auto_transcoding =
              source_encode &&
              sink_encode &&
              source_encode !== sink_encode &&
              !use_manual_transcoding;
            const encoder_properties =
              stream.configuration.encoder_properties;
            const decoder_properties =
              stream.configuration.decoder_properties;
            const codec_property_fields = {
              ...(encoder_properties &&
              Object.keys(encoder_properties).length > 0
                ? { encoder_properties }
                : {}),
              ...(decoder_properties &&
              Object.keys(decoder_properties).length > 0
                ? { decoder_properties }
                : {}),
            };
            return {
              type: "video",
              source_encode: source_encode,
              encode: sink_encode,
              height: Number(stream.configuration.size.height),
              width: Number(stream.configuration.size.width),
              frame_interval: stream.configuration.interval,
              ...(Number(stream.configuration.bit_depth) > 0
                ? { bit_depth: Number(stream.configuration.bit_depth) }
                : {}),
              ...(use_manual_transcoding
                ? {
                    source_configuration: {
                      type: "manual",
                      encoder: stream.configuration.encoder ?? "",
                      ...(stream.configuration.decoder
                        ? { decoder: stream.configuration.decoder }
                        : {}),
                      ...codec_property_fields,
                    },
                  }
                : use_auto_transcoding &&
                    Object.keys(codec_property_fields).length > 0
                  ? {
                      source_configuration: {
                        type: "auto",
                        ...codec_property_fields,
                      },
                    }
                  : {}),
            };
          }
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
            disable_lazy: Boolean(
              stream.extended_configuration.disable_lazy
            ),
            disable_recording: Boolean(
              stream.extended_configuration.disable_recording
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
      restartRequiredLegend: "Changing this requires a stream restart",
    };
  },
});
</script>

<style>
.restart-required-legend {
  color: #a60;
  margin: 0.5em 0;
}
.restart-required-marker {
  color: #a60;
  margin-right: 0.25em;
  cursor: help;
  text-decoration: none;
}
.collapsible {
  margin: 0.5em 0;
}
.collapsible > summary {
  cursor: pointer;
  font-weight: bold;
  margin: 0.5em 0;
}
.plugin-control-group,
.pipeline-plugin-group {
  margin: 0.5em 0 0.5em 1em;
}
.plugin-control-group > summary,
.pipeline-plugin-group > summary {
  cursor: pointer;
  font-weight: 600;
}
.plugin-control-item,
.pipeline-control-item {
  margin: 0.4em 0 0.4em 0.5em;
}
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
