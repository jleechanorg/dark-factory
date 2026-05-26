function dollarsToCents(value) {
  const valueString = `${value}`.replace(/,/g, '').trim();
  const parsed = Number.parseFloat(valueString);
  if (Number.isNaN(parsed)) {
    return NaN;
  }
  return Math.round(parsed * 100);
}

function centsToDollars(valueInCents) {
  if (typeof valueInCents !== 'number' || Number.isNaN(valueInCents)) {
    return 0;
  }
  return Number((valueInCents / 100).toFixed(2));
}

function centsToDisplay(valueInCents) {
  return centsToDollars(valueInCents).toFixed(2);
}

module.exports = {
  dollarsToCents,
  centsToDollars,
  centsToDisplay,
};
