/* Presentation data only. Replace this object to change the examples. */
const chartDemo = {
  bars: [18, 28, 23, 45, 38, 57, 44, 66, 54, 73, 62, 84],
  labels: ['01', '02', '03', '04', '05', '06', '07', '08', '09', '10', '11', '12'],
  stacks: [{ label: 'Lun', values: [12, 20, 30] }, { label: 'Mar', values: [24, 28, 35] }, { label: 'Mié', values: [18, 26, 24] }],
  series: ['Base', 'Lecturas', 'Eventos'],
};
const barRoot = document.querySelector('#bar-demo');
const barLabels = document.querySelector('#bar-labels');
const max = Math.max(1, ...chartDemo.bars);
chartDemo.bars.forEach((value, i) => {
  const bar = document.createElement('button');
  bar.type = 'button'; bar.className = 'rg-bar';
  bar.style.setProperty('--value', `${value / max * 100}%`);
  bar.setAttribute('aria-label', `${chartDemo.labels[i]}: ${value} eventos de ejemplo`);
  bar.setAttribute('aria-pressed', 'false');
  const tip = document.createElement('span'); tip.className = 'rg-bar-tip'; tip.textContent = `${chartDemo.labels[i]} · ${value}`;
  const column = document.createElement('span'); column.className = 'rg-bar-column';
  for (const name of ['cap', 'sliver', 'body']) { const span = document.createElement('span'); span.className = `rg-bar-${name}`; column.append(span); }
  bar.append(tip, column);
  bar.addEventListener('click', () => { for (const other of barRoot.children) other.setAttribute('aria-pressed', String(other === bar)); document.querySelector('#chart-status').textContent = `Día ${chartDemo.labels[i]}: ${value} eventos de ejemplo.`; });
  barRoot.append(bar);
  const label = document.createElement('span'); label.textContent = chartDemo.labels[i]; barLabels.append(label);
});
const stackMax = Math.max(1, ...chartDemo.stacks.map(day => day.values.reduce((a, b) => a + b, 0)));
chartDemo.stacks.forEach(day => {
  const total = day.values.reduce((a, b) => a + b, 0);
  const column = document.createElement('div'); column.className = 'rg-stack'; column.style.setProperty('--value', `${total / stackMax * 100}%`);
  column.setAttribute('role', 'img'); column.setAttribute('aria-label', `${day.label}: ${day.values.map((v, i) => `${chartDemo.series[i]} ${v}`).join(', ')}`);
  [...day.values].reverse().forEach((value, i) => { const span = document.createElement('span'); span.style.setProperty('--weight', value); span.style.setProperty('--shade', `var(--rg-color-stack-${4 + i})`); column.append(span); });
  document.querySelector('#stack-demo').append(column);
  const label = document.createElement('span'); label.textContent = `${day.label} · ${total}`; document.querySelector('#stack-labels').append(label);
});
chartDemo.series.forEach((name, i) => { const li = document.createElement('li'); const swatch = document.createElement('i'); swatch.style.setProperty('--shade', `var(--rg-color-stack-${6 - i})`); li.append(swatch, name); document.querySelector('#stack-legend').append(li); });
for (const card of document.querySelectorAll('.rg-spotlight')) {
  card.addEventListener('pointermove', event => {
    if (matchMedia('(prefers-reduced-motion: reduce)').matches || event.pointerType === 'touch') return;
    const bounds = card.getBoundingClientRect();
    card.style.setProperty('--spot-x', `${event.clientX - bounds.left}px`);
    card.style.setProperty('--spot-y', `${event.clientY - bounds.top}px`);
  });
}
