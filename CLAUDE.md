# ai-agent — инструкции для Claude Code

Проект: модульный Rust CLI AI-агент (src/), TUI (ratatui), планы Luck,
веб-фронтенд, тестовый TUI-драйвер через tmux.

## Луп самосовершенствования

Когда пользователь просит «запусти луп», «самосовершенствование», «прогони тест»,
«improve», «проверь агента»:

1. Запусти пайп (дефолтный сценарий из 5 вопросов):
   ```
   python3 scripts/self_improve.py
   ```
   Или со своим сценарием:
   ```
   python3 scripts/self_improve.py --questions "привет|найди TODO через grep" --target "привет|TODO"
   ```

2. Прочитай отчёт:
   - exit 0 — качество ок, задача выполнена, кратко отчитайся пользователю.
   - exit 1 — есть FAIL. Открой out/self_improve_<ts>.json и разбери причины.

3. Причины FAIL и что править:
   - «пустой ответ» / «ask error: tmux сессия мертва» → инфраструктура: проверь
     tmux, перезапусти сессию, увеличь --restart-every или уменьши его.
   - «ошибка провайдера» → модель/прокси/парсинг: проверь agent_config.json
     (провайдер/ключ), сеть, логи chat_logs/*.jsonl (stage=tool_result/llm_call).
   - «повтор предыдущего ответа» → деградация контекста: уменьши --restart-every
     (рестарт чаще) или добавь компакцию в цикле агента (src/agent.rs).
   - «нет ключевого слова» → поведение модели: правь системный промпт
     (src/main.rs, блок «Системный промпт»), усиль формулировку.
   - «слишком долго» → скорость: модель/прокси (kimi/minimax медленнее), либо
     увеличение --max-time.

4. После правки: cargo build && cargo test --lib (обязательно), затем перезапусти
   пайп. Не более 3 итераций на один прогон.

## Команды (ручные)

Драйвер TUI (tmux-сессия ai-tui-test):
```
python3 scripts/tui_driver.py start          # поднять TUI
python3 scripts/tui_driver.py ask "текст"    # ввод + ожидание ответа (печатает диалог)
python3 scripts/tui_driver.py read           # прочитать экран
python3 scripts/tui_driver.py stop           # Ctrl+C + kill сессии
python3 scripts/tui_driver.py status         # жива ли сессия
```
Батч-прогон с рестартами (для длинных сессий):
```
python3 scripts/batch_ask.py --questions "q1|q2|q3" --restart-every 10
```
Живой просмотр: tmux attach -t ai-tui-test (отсоединение Ctrl+B, D).

## Правила

- Модель/провайдер берутся из agent_config.json (в .gitignore, содержит ключ) —
  НЕ коммитить этот файл и не выводить ключи.
- Правки только в src/, scripts/, tests/ — не трогать незакоммиченные
  пользовательские файлы (run.sh, Arun.sh, backend/, Cargo.toml правки юзера).
- Тесты обязательны перед завершением: cargo test --lib, 0 warnings.
- Отчёты пайпа в out/ — можно коммитить (данные для анализа).
