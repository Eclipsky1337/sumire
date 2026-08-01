import { formatBytes } from "./format.js";

export function createTrafficCharts(elements, state) {
  function pushSpeedSample(download, upload) {
    state.speedHistory.download.push(download);
    state.speedHistory.upload.push(upload);
    if (state.speedHistory.download.length > 60) state.speedHistory.download.shift();
    if (state.speedHistory.upload.length > 60) state.speedHistory.upload.shift();
  }

  function drawTrafficCharts() {
    drawSpeedChart();
    drawTrafficDonut();
  }

  function prepareCanvas(canvas) {
    const bounds = canvas.getBoundingClientRect();
    if (bounds.width <= 0 || bounds.height <= 0) return null;
    const ratio = window.devicePixelRatio || 1;
    const pixelWidth = Math.round(bounds.width * ratio);
    const pixelHeight = Math.round(bounds.height * ratio);
    if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
      canvas.width = pixelWidth;
      canvas.height = pixelHeight;
    }
    const context = canvas.getContext("2d");
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, bounds.width, bounds.height);
    return { context, width: bounds.width, height: bounds.height };
  }

  function drawSpeedChart() {
    const prepared = prepareCanvas(elements.speedChart);
    if (!prepared) return;
    const { context, width, height } = prepared;
    const padding = { top: 16, right: 8, bottom: 20, left: 8 };
    const chartWidth = width - padding.left - padding.right;
    const chartHeight = height - padding.top - padding.bottom;
    const download = state.speedHistory.download;
    const upload = state.speedHistory.upload;
    const maximum = Math.max(1024, ...download, ...upload);

    context.lineWidth = 1;
    context.strokeStyle = "rgba(137, 162, 154, .13)";
    for (let row = 0; row <= 4; row++) {
      const y = padding.top + chartHeight * row / 4;
      context.beginPath();
      context.moveTo(padding.left, y);
      context.lineTo(width - padding.right, y);
      context.stroke();
    }

    drawSpeedSeries(context, download, maximum, padding, chartWidth, chartHeight, "#55e6a5", "rgba(85, 230, 165, .13)");
    drawSpeedSeries(context, upload, maximum, padding, chartWidth, chartHeight, "#68a9ff", "rgba(104, 169, 255, .09)");

    context.fillStyle = "#6f8c82";
    context.font = "10px ui-monospace, monospace";
    context.fillText(`${formatBytes(maximum)}/s`, padding.left, 10);
    context.textAlign = "right";
    context.fillText("最近 2 分钟", width - padding.right, height - 3);
    context.textAlign = "left";
  }

  function drawSpeedSeries(context, values, maximum, padding, chartWidth, chartHeight, strokeColor, fillColor) {
    if (values.length === 0) return;
    const points = values.map((value, index) => ({
      x: padding.left + (values.length === 1 ? chartWidth : chartWidth * index / (values.length - 1)),
      y: padding.top + chartHeight - Math.min(1, value / maximum) * chartHeight,
    }));
    context.beginPath();
    context.moveTo(points[0].x, points[0].y);
    for (let index = 1; index < points.length; index++) {
      const previous = points[index - 1];
      const current = points[index];
      const middleX = (previous.x + current.x) / 2;
      context.bezierCurveTo(middleX, previous.y, middleX, current.y, current.x, current.y);
    }
    context.lineWidth = 2;
    context.strokeStyle = strokeColor;
    context.stroke();
    context.lineTo(points[points.length - 1].x, padding.top + chartHeight);
    context.lineTo(points[0].x, padding.top + chartHeight);
    context.closePath();
    context.fillStyle = fillColor;
    context.fill();
  }

  function drawTrafficDonut() {
    const prepared = prepareCanvas(elements.trafficDonut);
    if (!prepared) return;
    const { context, width, height } = prepared;
    const downloaded = state.traffic?.downloaded_bytes || 0;
    const uploaded = state.traffic?.uploaded_bytes || 0;
    const total = downloaded + uploaded;
    const radius = Math.max(10, Math.min(width, height) / 2 - 12);
    const centerX = width / 2;
    const centerY = height / 2;
    const start = -Math.PI / 2;

    context.lineWidth = 15;
    context.lineCap = "round";
    context.strokeStyle = "rgba(137, 162, 154, .13)";
    context.beginPath();
    context.arc(centerX, centerY, radius, 0, Math.PI * 2);
    context.stroke();
    if (total <= 0) return;

    const downloadAngle = Math.PI * 2 * downloaded / total;
    context.lineCap = "butt";
    context.strokeStyle = "#55e6a5";
    context.beginPath();
    context.arc(centerX, centerY, radius, start, start + downloadAngle);
    context.stroke();
    context.strokeStyle = "#68a9ff";
    context.beginPath();
    context.arc(centerX, centerY, radius, start + downloadAngle, start + Math.PI * 2);
    context.stroke();
  }

  return { drawTrafficCharts, pushSpeedSample };
}
