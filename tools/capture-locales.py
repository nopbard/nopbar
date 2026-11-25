"""Add capture labels without reformatting existing catalog entries."""
import json
import re
from pathlib import Path

labels = {
    "ko":"받아 적기", "en":"Quick capture", "ar":"تدوين سريع", "bg":"Бързо записване", "bn":"দ্রুত নোট", "cs":"Rychlý zápis", "da":"Hurtig note", "de":"Schnellnotiz", "el":"Γρήγορη σημείωση", "es":"Nota rápida", "et":"Kiirmärge", "fa":"یادداشت سریع", "fi":"Pikamuistiinpano", "fil":"Mabilis na tala", "fr":"Note rapide", "he":"רישום מהיר", "hi":"त्वरित नोट", "hr":"Brza bilješka", "hu":"Gyors jegyzet", "id":"Catatan cepat", "it":"Nota rapida", "ja":"クイックメモ", "lt":"Greitas užrašas", "lv":"Ātrā piezīme", "ms":"Nota pantas", "nb":"Hurtignotat", "nl":"Snelle notitie", "pl":"Szybka notatka", "pt-br":"Nota rápida", "pt-pt":"Nota rápida", "ro":"Notă rapidă", "ru":"Быстрая заметка", "sk":"Rýchla poznámka", "sl":"Hitra beležka", "sr":"Брза белешка", "sv":"Snabbanteckning", "th":"บันทึกด่วน", "tr":"Hızlı not", "uk":"Швидка нотатка", "vi":"Ghi chú nhanh", "zh-cn":"快速记录", "zh-tw":"快速記錄"
}
texts = {
"capture.inbox": ("인박스", "Inbox"),
"capture.placeholder": ("적기", "Write"),
"capture.destination": ("Tab 보낼 곳", "Tab destination"),
"capture.change_placeholder": ("시각 입력…", "Enter time…"),
"widgets.notes_reminders": ("메모 시간 알림", "Note reminders"),
"widgets.notes_reminders.hint": ("메모에 적은 시각에 노움이 알려 줌.", "Reminders for times written in notes."),
"widgets.notes_lead": ("미리 알림", "Advance notice"),
"widgets.notes_date_hour": ("날짜만 있을 때", "Date only"),
"remind.on_time": ("정각", "On time"),
"remind.five_before": ("5분 전", "5 min before"),
"remind.ten_before": ("10분 전", "10 min before"),
"remind.nine": ("오전 9시", "9 AM"),
"remind.ten": ("오전 10시", "10 AM"),
"remind.off": ("알리지 않음", "No reminder"),
"remind.change": ("시간 바꾸기", "Change time"),
"remind.disable": ("알림 끄기", "Disable"),
"remind.complete": ("완료", "Done"),
"remind.snooze": ("10분 뒤", "In 10 min"),
"remind.completed_count": ("완료 {n}", "Done {n}"),
"remind.missed_count": ("놓친 알림 {n}", "Missed reminders {n}"),
"remind.in_minutes": ("{n}분 뒤 {text}", "In {n} min: {text}"),
}
root = Path(__file__).resolve().parents[1] / "dock/locales"
for path in root.glob("*.json"):
    s = path.read_text(encoding="utf-8")
    tag = path.stem
    if tag not in labels:
        raise ValueError(tag)
    s = re.sub(r'("shortcuts.notes":\s*)"(?:[^"\\]|\\.)*"', lambda m: m[1] + json.dumps(labels[tag], ensure_ascii=False), s)
    # The compact shortcut label is also its tooltip outside Korean/English.
    hint = "받아 적기 열기, 빈 막대에서 다시 누르면 메모 소환." if tag == "ko" else "Open capture; press again when empty to summon notes." if tag == "en" else labels[tag]
    s = re.sub(r'("shortcuts.notes.hint":\s*)"(?:[^"\\]|\\.)*"', lambda m: m[1] + json.dumps(hint, ensure_ascii=False), s)
    present = json.loads(s)
    additions = {k: v[0 if tag == "ko" else 1] for k, v in texts.items() if k not in present}
    if additions:
        s = s.rstrip().removesuffix("}").rstrip() + ",\n" + json.dumps(additions, ensure_ascii=False, indent=2)[2:]
    path.write_text(s.rstrip()+"\n",encoding="utf-8")
