import { Config } from "@remotion/cli/config";

Config.setVideoImageFormat("jpeg");
Config.setPixelFormat("yuv420p");
Config.setCodec("h264");
// LinkedIn re-encodes anyway; this keeps the upload crisp without a huge file.
Config.setCrf(18);
Config.overrideWebpackConfig((config) => config);
