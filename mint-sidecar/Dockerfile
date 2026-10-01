# ZCode 活动套餐验证码令牌纯后台铸造器（happy-dom 跑阿里云 SDK）
FROM node:22-alpine
WORKDIR /app
COPY package.json ./
RUN npm install --omit=dev --no-audit --no-fund
COPY mint.js ./
# happy-dom 需要一个可写的 HOME 与时区无关的运行环境；非 root 运行
USER node
ENV NODE_ENV=production
CMD ["node", "mint.js"]
