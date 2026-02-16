<template>
  <div>
    <input
      type="range"
      :min="slider.min"
      :max="slider.max"
      :value="val"
      :step="slider.step"
      :id="'range-input-' + name"
      @change="$emit('onchange', val)"
      @input="val = Number(($event.target as HTMLInputElement).value)"
      class="slider"
    />
    <span>{{ val }}</span>
  </div>
</template>

<script lang="ts">
import { defineComponent } from "vue";

export default defineComponent({
  name: "V4lSlider",
  props: {
    slider: {
      type: Object,
      required: true,
    },
    name: {
      type: String,
      required: true,
    },
  },
  emits: ["onchange"],
  data() {
    return {
      val: this.slider.value,
    };
  },
});
</script>
