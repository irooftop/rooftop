<script setup lang="ts">
// The home page's hero: the scene, wordmark and tagline of iroof.top. scene.svg is the
// app's own (web/index.html), copied by .vitepress/assets.mjs.
import { useData } from "vitepress";
import scene from "./scene.svg?raw";

const { frontmatter } = useData();
</script>

<template>
  <div class="ir-hero">
    <div class="ir-scene" v-html="scene" />
    <h1 class="ir-wordmark" aria-label="iRoof.top">i<span class="amp">&amp;</span>roof.top</h1>
    <p class="ir-tagline">{{ frontmatter.hero?.text }}.</p>
    <p class="ir-lead">{{ frontmatter.hero?.tagline }}</p>
  </div>
</template>
