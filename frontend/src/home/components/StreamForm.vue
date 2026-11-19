<template>
  <form>
    <p>
      <label>Name: </label>
      <input
        name="name"
        type="text"
        autocomplete="off"
        v-model="stream_setting.name"
      />
    </p>

    <div>
      <label>Capture format: </label>
      <select
        v-model="stream_setting.configuration.source_encode"
        :disabled="stream_setting.source == 'Redirect'"
      >
        <option
          v-for="encode in stream_options.encoders"
          :key="encode"
          :value="encode"
        >
          {{ encode }}
        </option>
      </select>
    </div>
    <div>
      <label>Size: </label>
      <select
        v-model="stream_setting.configuration.size"
        :disabled="stream_setting.source == 'Redirect'"
      >
        <option
          v-for="size in stream_options.sizes"
          :key="size.width + 'x' + size.height"
          :value="{ width: size.width, height: size.height }"
        >
          {{ size.width }} x {{ size.height }}
        </option>
      </select>
    </div>
    <div>
      <label>Bit depth: </label>
      <select
        v-model="stream_setting.configuration.bit_depth"
        :disabled="stream_setting.source == 'Redirect' || !bitDepthAvailable"
      >
        <option v-if="!bitDepthAvailable" :value="undefined">
          Not available
        </option>
        <option v-else :value="undefined">Auto</option>
        <option
          v-for="depth in stream_options.depths"
          :key="depth.bit_depth"
          :value="depth.bit_depth"
        >
          {{ depth.bit_depth }}-bit
        </option>
      </select>
    </div>
    <div>
      <label>FPS: </label>
      <select
        v-model="stream_setting.configuration.interval"
        :disabled="stream_setting.source == 'Redirect'"
      >
        <option
          v-for="interval in stream_options.intervals"
          :key="interval.denominator + '/' + interval.numerator"
          :value="interval"
        >
          {{ +(interval.denominator / interval.numerator).toFixed(2) }}
        </option>
      </select>
    </div>
    <div>
      <label>Output format: </label>
      <select
        v-model="stream_setting.configuration.encode"
        :disabled="stream_setting.source == 'Redirect'"
      >
        <option
          v-for="encode in sinkEncoders"
          :key="encode"
          :value="encode"
        >
          {{ encode }}
        </option>
      </select>
    </div>
    <div v-if="isTranscoding" class="transcoding-settings">
      <div>
        <label>Transcoding: </label>
        <select
          v-if="isCompressedSource"
          v-model="stream_setting.configuration.transcoding_mode"
        >
          <option value="auto">Automatic</option>
          <option value="manual">Manual</option>
        </select>
        <span v-else class="transcoding-fixed-mode">Manual</span>
      </div>
      <template v-if="showManualTranscodingControls">
        <div v-if="isCompressedSource">
          <label>Decoder: </label>
          <select v-model="stream_setting.configuration.decoder">
            <option value="">
              Default ({{ defaultDecoderName }})
            </option>
            <option
              v-for="decoder in decodersForSource"
              :key="decoder.name"
              :value="decoder.name"
            >
              {{ decoder.name }}
              <template v-if="decoder.nick && decoder.nick !== decoder.name">
                — {{ decoder.nick }}
              </template>
            </option>
          </select>
        </div>
        <div v-if="!isRawSink">
          <label>Encoder: </label>
          <select v-model="stream_setting.configuration.encoder">
            <option
              v-for="encoder in encodersForSink"
              :key="encoder.name"
              :value="encoder.name"
            >
              {{ encoder.name }}
              <template v-if="encoder.nick && encoder.nick !== encoder.name">
                — {{ encoder.nick }}
              </template>
            </option>
          </select>
          <div v-if="selectedEncoder" class="encoder-details">
            <h5>
              {{ selectedEncoder.nick || selectedEncoder.name }}
              <InfoCard
                v-if="selectedEncoder.blurb"
                :description="selectedEncoder.blurb"
              />
              <a
                v-if="selectedEncoder.docs_url"
                class="encoder-docs"
                :href="selectedEncoder.docs_url"
                target="_blank"
                rel="noopener noreferrer"
                >docs</a
              >
            </h5>
            <p class="encoder-code">
              <code>{{ selectedEncoder.name }}</code>
              <InfoCard
                v-if="selectedEncoder.factory"
                :key="selectedEncoder.name + '-factory'"
                label="Factory"
                aria-label="Factory details"
                wide
              >
                <dl>
                  <dt>Name</dt>
                  <dd><code>{{ selectedEncoder.factory.name }}</code></dd>
                  <dt>Long-name</dt>
                  <dd>{{ selectedEncoder.factory.long_name }}</dd>
                  <dt>Klass</dt>
                  <dd>{{ selectedEncoder.factory.klass }}</dd>
                  <dt>Description</dt>
                  <dd>{{ selectedEncoder.factory.description }}</dd>
                  <dt>Author</dt>
                  <dd>{{ selectedEncoder.factory.author }}</dd>
                  <dt>Rank</dt>
                  <dd>
                    {{ selectedEncoder.factory.rank }}
                    ({{ selectedEncoder.factory.rank_value }})
                  </dd>
                </dl>
              </InfoCard>
              <InfoCard
                v-if="selectedEncoder.plugin"
                :key="selectedEncoder.name + '-plugin'"
                label="Plugin"
                aria-label="Plugin details"
                wide
              >
                <dl>
                  <dt>Name</dt>
                  <dd><code>{{ selectedEncoder.plugin.name }}</code></dd>
                  <dt>Description</dt>
                  <dd>{{ selectedEncoder.plugin.description }}</dd>
                  <dt>Filename</dt>
                  <dd>{{ selectedEncoder.plugin.filename || "—" }}</dd>
                  <dt>Version</dt>
                  <dd>{{ selectedEncoder.plugin.version }}</dd>
                  <dt>License</dt>
                  <dd>{{ selectedEncoder.plugin.license }}</dd>
                  <dt>Source module</dt>
                  <dd>{{ selectedEncoder.plugin.source }}</dd>
                  <dt>Source release date</dt>
                  <dd>{{ selectedEncoder.plugin.release_date || "—" }}</dd>
                  <dt>Binary package</dt>
                  <dd>{{ selectedEncoder.plugin.package }}</dd>
                  <dt>Origin URL</dt>
                  <dd>
                    <a
                      v-if="isHttpUrl(selectedEncoder.plugin.origin)"
                      :href="selectedEncoder.plugin.origin"
                      target="_blank"
                      rel="noopener noreferrer"
                      >{{ selectedEncoder.plugin.origin }}</a
                    >
                    <template v-else>{{ selectedEncoder.plugin.origin }}</template>
                  </dd>
                  <dt>Loaded</dt>
                  <dd>{{ selectedEncoder.plugin.is_loaded ? "yes" : "no" }}</dd>
                </dl>
              </InfoCard>
            </p>
            <p v-if="stream_options.gstreamer" class="encoder-build">
              {{ stream_options.gstreamer.version_string }}
            </p>
          </div>
        </div>
      </template>
    </div>
    <div>
      <label>Thermal: </label>
      <input
        type="checkbox"
        v-model="stream_setting.extended_configuration.thermal"
      />
    </div>
    <div>
      <label>Disable Mavlink: </label>
      <input
        type="checkbox"
        v-model="stream_setting.extended_configuration.disable_mavlink"
      />
    </div>
    <div>
      <label>Disable Zenoh: </label>
      <input
        type="checkbox"
        v-model="stream_setting.extended_configuration.disable_zenoh"
      />
    </div>
    <div>
      <label>Disable Thumbnails: </label>
      <input
        type="checkbox"
        v-model="stream_setting.extended_configuration.disable_thumbnails"
      />
    </div>
    <div>
      <label>Disable Lazy: </label>
      <input
        type="checkbox"
        v-model="stream_setting.extended_configuration.disable_lazy"
      />
    </div>
    <div>
      <label>Disable Recording: </label>
      <input
        type="checkbox"
        v-model="stream_setting.extended_configuration.disable_recording"
      />
    </div>

    <p>
      <label>Endpoints: </label>
      <input
        type="text"
        autocomplete="off"
        placeholder="udp://0.0.0.0:5600"
        v-model="stream_setting.endpoints"
      />
    </p>
    <button type="button" @click="$emit('onconfigure', stream_setting)">
      Configure stream
    </button>
  </form>
</template>

<script lang="ts">
import { defineComponent } from "vue";
import InfoCard from "./InfoCard.vue";

export default defineComponent({
  name: "StreamForm",
  components: {
    InfoCard,
  },
  props: {
    device: {
      type: Object,
      required: true,
    },
    streams: {
      type: Object,
      required: true,
    },
  },
  emits: ["onconfigure"],
  mounted() {
    this.stream_options.encoders = this.device.formats.map((format: any) =>
      this.encodeToStr(format.encode)
    );
    this.loadEncoders();
    this.loadDecoders();
  },
  watch: {
    streams: {
      handler(streams: any[]) {
        this.stream = streams.filter(
          (stream: any) =>
            (stream.video_and_stream.video_source.Local &&
              stream.video_and_stream.video_source.Local.device_path ==
                this.device.source) ||
            (stream.video_and_stream.video_source.Gst &&
              stream.video_and_stream.video_source.Gst.source.Fake ==
                this.device.source)
        )[0];
        if (!this.stream) {
          return;
        }

        switch (
          this.stream.video_and_stream.stream_information.configuration.type
        ) {
          case "redirect":
            break;
          default: {
            const configuration =
              this.stream.video_and_stream.stream_information.configuration;
            this.stream_setting.configuration.source_encode =
              configuration.source_encode ?? configuration.encode;
            this.stream_setting.configuration.encode = configuration.encode;
            this.stream_setting.configuration.size = {
              height:
                this.stream.video_and_stream.stream_information.configuration
                  .height,
              width:
                this.stream.video_and_stream.stream_information.configuration
                  .width,
            };
            this.stream_setting.configuration.interval =
              this.stream.video_and_stream.stream_information.configuration.frame_interval;
            this.stream_setting.configuration.bit_depth =
              this.stream.video_and_stream.stream_information.configuration.bit_depth;
            const source_configuration = configuration.source_configuration;
            if (source_configuration?.type === "manual") {
              this.stream_setting.configuration.transcoding_mode = "manual";
              this.stream_setting.configuration.encoder =
                source_configuration.encoder;
              this.stream_setting.configuration.decoder =
                source_configuration.decoder ?? "";
              this.stream_setting.configuration.encoder_properties =
                source_configuration.encoder_properties ?? {};
              this.stream_setting.configuration.decoder_properties =
                source_configuration.decoder_properties ?? {};
              this.ensureEncoderIsListed();
              this.ensureDecoderIsListed();
            } else if (source_configuration?.type === "auto") {
              this.stream_setting.configuration.transcoding_mode = "auto";
              this.stream_setting.configuration.encoder_properties =
                source_configuration.encoder_properties ?? {};
              this.stream_setting.configuration.decoder_properties =
                source_configuration.decoder_properties ?? {};
            } else if (
              configuration.source_encode &&
              configuration.encode &&
              configuration.source_encode !== configuration.encode
            ) {
              this.stream_setting.configuration.transcoding_mode = "auto";
            }
          }
        }

        this.stream_setting.endpoints = this.stream.video_and_stream
          .stream_information.endpoints
          ? this.stream.video_and_stream.stream_information.endpoints.join(", ")
          : "";
        this.stream_setting.extended_configuration.thermal = Boolean(
          this.stream.video_and_stream.stream_information
            .extended_configuration?.thermal
        );
        this.stream_setting.extended_configuration.disable_mavlink = Boolean(
          this.stream.video_and_stream.stream_information
            .extended_configuration?.disable_mavlink
        );
        this.stream_setting.extended_configuration.disable_zenoh = Boolean(
          this.stream.video_and_stream.stream_information
            .extended_configuration?.disable_zenoh
        );
        this.stream_setting.extended_configuration.disable_thumbnails = Boolean(
          this.stream.video_and_stream.stream_information
            .extended_configuration?.disable_thumbnails
        );
        this.stream_setting.extended_configuration.disable_lazy = Boolean(
          this.stream.video_and_stream.stream_information
            .extended_configuration?.disable_lazy
        );
        this.stream_setting.extended_configuration.disable_recording = Boolean(
          this.stream.video_and_stream.stream_information
            .extended_configuration?.disable_recording
        );
      },
      deep: true,
    },
    stream_setting: {
      handler(stream_setting: any) {
        console.log(JSON.stringify(stream_setting, undefined, 2));

        switch (stream_setting.configuration.type) {
          case "redirect":
            break;
          default: {
            this.stream_options.encoders = this.device.formats.map(
              (format: any) => this.encodeToStr(format.encode)
            );

            const sink_encoders = this.sinkEncoders;
            if (!stream_setting.configuration.encode && stream_setting.configuration.source_encode) {
              this.stream_setting.configuration.encode =
                stream_setting.configuration.source_encode;
            } else if (
              stream_setting.configuration.encode &&
              !sink_encoders.includes(stream_setting.configuration.encode)
            ) {
              this.stream_setting.configuration.encode =
                stream_setting.configuration.source_encode;
            }
            if (
              stream_setting.configuration.transcoding_mode === "manual" &&
              this.isTranscoding
            ) {
              this.ensureEncoderIsListed();
            }

            this.stream_options.sizes = this.device.formats
              .filter(
                (format: any) =>
                  this.encodeToStr(format.encode) ==
                  stream_setting.configuration.source_encode
              )
              .map((format: any) => format.sizes)[0]
              // Sort width by preference
              ?.sort(
                (size1: any, size2: any) =>
                  10 * size2.width +
                  size2.height -
                  (10 * size1.width + size1.height)
              );

            console.log(this.stream_options.sizes);

            const chosen_size = stream_setting.configuration.size;
            if (chosen_size == undefined) {
              return;
            }

            const chosen = this.stream_options.sizes?.filter(
              (size: any) =>
                size.width == chosen_size.width &&
                size.height == chosen_size.height
            )[0];
            this.stream_options.depths = chosen?.depths ?? [];
            const chosen_bit_depth = stream_setting.configuration.bit_depth;
            if (
              chosen_bit_depth != null &&
              !this.stream_options.depths.some(
                (depth: any) => depth.bit_depth == chosen_bit_depth
              )
            ) {
              this.stream_setting.configuration.bit_depth = undefined;
            }
            const selected_depth =
              this.stream_options.depths.find(
                (depth: any) =>
                  depth.bit_depth ==
                  this.stream_setting.configuration.bit_depth
              ) ??
              this.stream_options.depths.find(
                (depth: any) => depth.bit_depth == 10
              ) ??
              this.stream_options.depths[0];
            this.stream_options.intervals =
              selected_depth?.intervals ?? chosen?.intervals;
            const chosen_interval = stream_setting.configuration.interval;
            if (
              chosen_interval != null &&
              Array.isArray(this.stream_options.intervals) &&
              !this.stream_options.intervals.some(
                (interval: any) =>
                  interval.numerator == chosen_interval.numerator &&
                  interval.denominator == chosen_interval.denominator
              )
            ) {
              this.stream_setting.configuration.interval =
                this.stream_options.intervals[0];
            }
          }
        }
      },
      deep: true,
    },
  },
  computed: {
    bitDepthAvailable(): boolean {
      return (
        Array.isArray(this.stream_options.depths) &&
        this.stream_options.depths.length > 0
      );
    },
    sinkEncoders(): string[] {
      const source_encode = this.stream_setting.configuration.source_encode;
      if (!source_encode) {
        return [];
      }
      const sink_encoders = [source_encode];
      if (this.canTranscodeSource) {
        if (this.isCompressedSource) {
          for (const raw_encode of ["NV12", "YUYV", "RGB"]) {
            if (!sink_encoders.includes(raw_encode)) {
              sink_encoders.push(raw_encode);
            }
          }
        }
        for (const encode of Object.keys(this.stream_options.encodings)) {
          if (!sink_encoders.includes(encode)) {
            sink_encoders.push(encode);
          }
        }
      }
      return sink_encoders;
    },
    canTranscodeSource(): boolean {
      if (this.stream_setting.source === "Redirect") {
        return false;
      }
      const source_encode = this.stream_setting.configuration.source_encode;
      return [
        "NV12",
        "YUYV",
        "RGB",
        "MJPG",
        "H264",
        "H265",
      ].includes(source_encode);
    },
    isRawSource(): boolean {
      return ["NV12", "YUYV", "RGB"].includes(
        this.stream_setting.configuration.source_encode
      );
    },
    isRawSink(): boolean {
      return ["NV12", "YUYV", "RGB"].includes(
        this.stream_setting.configuration.encode
      );
    },
    isTranscoding(): boolean {
      const source_encode = this.stream_setting.configuration.source_encode;
      const sink_encode = this.stream_setting.configuration.encode;
      if (!source_encode || !sink_encode || source_encode === sink_encode) {
        return false;
      }
      if (!this.canTranscodeSource) {
        return false;
      }
      if (this.isRawSource) {
        return this.encodersForSink.length > 0;
      }
      return this.isCompressedSource;
    },
    isCompressedSource(): boolean {
      return ["MJPG", "H264", "H265"].includes(
        this.stream_setting.configuration.source_encode
      );
    },
    showManualTranscodingControls(): boolean {
      if (!this.isTranscoding) {
        return false;
      }
      if (this.isRawSource) {
        return true;
      }
      return this.stream_setting.configuration.transcoding_mode === "manual";
    },
    defaultDecoderName(): string {
      switch (this.stream_setting.configuration.source_encode) {
        case "MJPG":
          return "jpegdec";
        case "H264":
          return "avdec_h264";
        case "H265":
          return "avdec_h265";
        default:
          return "";
      }
    },
    encodersForSink(): any[] {
      const sink_encode = this.stream_setting.configuration.encode;
      return this.stream_options.encodings[sink_encode] ?? [];
    },
    decodersForSource(): any[] {
      const source_encode = this.stream_setting.configuration.source_encode;
      if (!source_encode) {
        return [];
      }
      const listed = this.stream_options.decodings[source_encode] ?? [];
      return listed.filter(
        (decoder: any) => decoder.name !== this.defaultDecoderName
      );
    },
    selectedEncoder(): any | undefined {
      const name = this.stream_setting.configuration.encoder;
      return this.encodersForSink.find(
        (encoder: any) => encoder.name === name
      );
    },
  },
  methods: {
    encodeToStr(encode: any): string {
      return typeof encode == "object"
        ? (Object.values(encode)[0] as string)
        : encode;
    },
    async loadEncoders() {
      try {
        const response = await fetch("/gst/encoders");
        if (response.ok) {
          const payload = await response.json();
          this.stream_options.encodings =
            payload.encodings && typeof payload.encodings === "object"
              ? payload.encodings
              : {};
          this.stream_options.gstreamer = payload.gstreamer;
        } else {
          this.stream_options.encodings = {};
        }
      } catch {
        this.stream_options.encodings = {};
      }
      this.ensureEncoderIsListed();
    },
    async loadDecoders() {
      try {
        const response = await fetch("/gst/decoders");
        if (response.ok) {
          const payload = await response.json();
          this.stream_options.decodings =
            payload.decodings && typeof payload.decodings === "object"
              ? payload.decodings
              : {};
        } else {
          this.stream_options.decodings = {};
        }
      } catch {
        this.stream_options.decodings = {};
      }
      this.ensureDecoderIsListed();
    },
    ensureEncoderIsListed() {
      if (this.isRawSink) {
        this.stream_setting.configuration.encoder = undefined;
        return;
      }
      const encoders = this.encodersForSink;
      const encoder = this.stream_setting.configuration.encoder;
      if (
        !encoder ||
        !encoders.some((listed: any) => listed.name === encoder)
      ) {
        this.stream_setting.configuration.encoder = encoders[0]?.name;
      }
    },
    ensureDecoderIsListed() {
      const decoder = this.stream_setting.configuration.decoder;
      if (
        decoder &&
        !this.decodersForSource.some((listed: any) => listed.name === decoder)
      ) {
        this.stream_setting.configuration.decoder = "";
      }
    },
    isHttpUrl(value: string): boolean {
      return /^https?:\/\//.test(value);
    },
  },
  data() {
    return {
      stream_setting: {
        name: this.device.source + " - " + this.device.name,
        source: this.device.source,
        endpoints: undefined as string | undefined,
        configuration: {
          source_encode: undefined as string | undefined,
          encode: undefined as string | undefined,
          transcoding_mode: "auto" as "auto" | "manual",
          encoder: undefined as string | undefined,
          decoder: "" as string,
          encoder_properties: {} as Record<string, unknown>,
          decoder_properties: {} as Record<string, unknown>,
          size: undefined as any,
          interval: undefined as any,
          bit_depth: undefined as number | undefined,
        },
        extended_configuration: {
          thermal: undefined as boolean | undefined,
          disable_mavlink: undefined as boolean | undefined,
          disable_zenoh: undefined as boolean | undefined,
          disable_thumbnails: undefined as boolean | undefined,
          disable_lazy: undefined as boolean | undefined,
          disable_recording: undefined as boolean | undefined,
        },
      },
      stream_options: {
        encoders: undefined as string[] | undefined,
        encodings: {} as Record<string, any[]>,
        decodings: {} as Record<string, any[]>,
        gstreamer: undefined as any,
        sizes: undefined as any[] | undefined,
        intervals: undefined as any[] | undefined,
        depths: [] as any[],
      },
      stream: undefined as any,
    };
  },
});
</script>

<style scoped>
select:disabled {
  color: #888;
  background-color: #e8e8e8;
  cursor: not-allowed;
}
.encoder-details {
  margin: 0.4em 0 0.8em 0;
  max-width: 40em;
}
.encoder-details h5 {
  margin: 0.3em 0;
}
.encoder-docs {
  margin-left: 0.5em;
  font-size: 0.8em;
  font-weight: normal;
}
.encoder-build {
  margin: 0.2em 0;
  color: #444;
  font-size: 0.85em;
}
.encoder-code {
  margin: 0.2em 0 0.5em;
}
.transcoding-settings > div {
  margin-bottom: 0.4em;
}
.transcoding-fixed-mode {
  color: #444;
}
</style>
