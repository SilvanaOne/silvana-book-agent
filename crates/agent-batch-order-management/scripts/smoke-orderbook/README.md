Смоук-сценарий **фазы 1** — не является workspace-пакетом (отдельный `npm install`).

`.env` ожидается в корне монорепо `batch-order-agent/.env` (на два уровня выше этого README).

Скопируйте из `.env.example` в корне репозитория.

```bash
cd batch-order-agent/scripts/smoke-orderbook
npm install
npm run smoke
```

Документ с шагами онбординга: `../../../task/docs/phase-1-onboarding-and-sdk.md`.
