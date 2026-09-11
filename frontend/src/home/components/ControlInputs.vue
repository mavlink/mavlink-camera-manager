<template>
  <div class="control">
    <h5>
      <abbr
        v-if="control.requires_restart"
        class="restart-required-marker"
        title="Changing this requires a stream restart"
        >[R]</abbr
      >
      {{ control.nick || control.name }}
      <InfoCard v-if="control.blurb" :description="control.blurb" />
      <a
        v-if="control.docs_url"
        class="control-docs"
        :href="control.docs_url"
        target="_blank"
        rel="noopener noreferrer"
        >docs</a
      >
    </h5>
    <div v-if="control.name === 'restart-stream'">
      <button type="button" @click="$emit('onchange', 1)">Restart stream</button>
    </div>
    <div v-else-if="control.configuration.Slider">
      <V4lSlider
        :slider="control.configuration.Slider"
        :name="control.id.toString()"
        @onchange="(value: any) => $emit('onchange', value)"
      />
    </div>
    <div v-else-if="control.configuration.Bool">
      <input
        type="checkbox"
        :checked="control.configuration.Bool.value == 1"
        @change="
          (event: Event) =>
            $emit(
              'onchange',
              (event.target as HTMLInputElement).checked ? 1 : 0
            )
        "
      />
      <label>On</label>
    </div>
    <div v-else-if="control.configuration.Menu">
      <select
        @change="
          (event: Event) =>
            $emit('onchange', Number((event.target as HTMLSelectElement).value))
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
    <div v-else-if="control.configuration.Flags">
      <label
        v-for="flag in control.configuration.Flags.flags"
        :key="flag.value"
        style="display: block"
      >
        <input
          type="checkbox"
          :checked="(control.configuration.Flags.value & flag.value) !== 0"
          @change="
            (event: Event) =>
              $emit(
                'onchange',
                nextFlagsValue(
                  flag.value,
                  (event.target as HTMLInputElement).checked
                )
              )
          "
        />
        {{ flag.name }}
      </label>
    </div>
    <p v-if="control.element" class="control-property-id">
      {{ control.element }}: {{ control.name }}
    </p>
  </div>
</template>

<script lang="ts">
import { defineComponent } from "vue";
import InfoCard from "./InfoCard.vue";
import V4lSlider from "./V4lSlider.vue";

export default defineComponent({
  name: "ControlInputs",
  components: {
    InfoCard,
    V4lSlider,
  },
  props: {
    control: {
      type: Object,
      required: true,
    },
  },
  emits: ["onchange"],
  methods: {
    nextFlagsValue(flagValue: number, checked: boolean): number {
      const current = Number(this.control.configuration.Flags.value);
      return checked ? current | flagValue : current & ~flagValue;
    },
  },
});
</script>

<style scoped>
.control-docs {
  margin-left: 0.5em;
  font-size: 0.8em;
  font-weight: normal;
}
.control-property-id {
  margin: 0.25em 0 0;
  color: #666;
  font-size: 0.75em;
}
.restart-required-marker {
  margin-right: 0.35em;
  color: #a60;
  text-decoration: none;
  cursor: help;
}
</style>
