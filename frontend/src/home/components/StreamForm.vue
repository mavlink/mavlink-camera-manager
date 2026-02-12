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
      <label>Encode: </label>
      <select
        v-model="stream_setting.configuration.encode"
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
          {{ interval.denominator / interval.numerator }}
        </option>
      </select>
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

export default defineComponent({
  name: "StreamForm",
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
            this.stream_setting.configuration.encode =
              this.stream.video_and_stream.stream_information.configuration.encode;
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

            this.stream_options.sizes = this.device.formats
              .filter(
                (format: any) =>
                  this.encodeToStr(format.encode) ==
                  stream_setting.configuration.encode
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

            this.stream_options.intervals = this.stream_options.sizes?.filter(
              (size: any) =>
                size.width == chosen_size.width &&
                size.height == chosen_size.height
            )[0]?.intervals;
          }
        }
      },
      deep: true,
    },
  },
  methods: {
    encodeToStr(encode: any): string {
      return typeof encode == "object"
        ? (Object.values(encode)[0] as string)
        : encode;
    },
  },
  data() {
    return {
      stream_setting: {
        name: this.device.source + " - " + this.device.name,
        source: this.device.source,
        endpoints: undefined as string | undefined,
        configuration: {
          encode: undefined as string | undefined,
          size: undefined as any,
          interval: undefined as any,
        },
        extended_configuration: {
          thermal: undefined as boolean | undefined,
          disable_mavlink: undefined as boolean | undefined,
          disable_zenoh: undefined as boolean | undefined,
          disable_thumbnails: undefined as boolean | undefined,
        },
      },
      stream_options: {
        encoders: undefined as string[] | undefined,
        sizes: undefined as any[] | undefined,
        intervals: undefined as any[] | undefined,
      },
      stream: undefined as any,
    };
  },
});
</script>
