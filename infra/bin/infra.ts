#!/usr/bin/env node
import * as cdk from "aws-cdk-lib";
import { ScreenMemoryStack } from "../lib/infra-stack";

const app = new cdk.App();
new ScreenMemoryStack(app, "ScreenMemoryStack", {
  // Deployment region. If you change it, update the MODEL_ID prefix in infra-stack.ts to match
  env: { region: "ap-northeast-1" },
});
