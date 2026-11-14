const usage={jobs:1240,succeeded:1190,failed:23,cancelled:7,in_progress:20,active_users:342,new_installations:83,active_installations:416};
const amounts={sales:250,gross_minor:125000,tax_minor:0,commission_minor:12500,publisher_minor:112500,reversed_publisher_minor:1500,net_publisher_minor:111000};
const apps=[{id:'a',title:'Document Studio',status:'published',usage},{id:'b',title:'Meeting Notes',status:'published',usage:{...usage,jobs:820,active_users:205}},{id:'c',title:'Legacy Search',status:'withdrawn',usage:{...usage,jobs:15,active_users:8}}];
window.PPMarketplace.analytics(document.querySelector('#root'),{pb:async path=>{
 if(path==='/v1/publishers'||path.startsWith('/v1/staff/publisher-analytics'))return{data:[{id:'pub_demo',display_name:'Example Studio'}]};
 const q=new URL(path,location.origin).searchParams,selected=q.get('listing_id'),days=[];
 for(let ms=Date.parse(q.get('from'));ms<Date.parse(q.get('to'));ms+=86400000)days.push(new Date(ms).toISOString().slice(0,10));
 const shown=selected?apps.filter(a=>a.id===selected):apps;
 return{data:{publisher:{id:'pub_demo',display_name:'Example Studio'},from:q.get('from'),to:q.get('to'),listing_id:selected,generated_at:new Date().toISOString(),usage,apps:shown,app_choices:apps,daily:days.map((day,i)=>({day,jobs:12+(i*17)%64})),finance:{status:'available',currencies:['USD','EUR','TRY'].map(currency=>({currency,...amounts})),apps:shown.map(a=>({listing_id:a.id,currency:'USD',...amounts})),daily:days.map((day,i)=>({day,listing_id:'a',currency:'USD',...amounts,net_publisher_minor:2500+(i*123)%4000})),publisher_balances:['USD','EUR','TRY'].map(currency=>({currency,payable_minor:90000,reserve_minor:10000,recoverable_minor:0,paid_out_minor:50000}))}}};
}},{staff:new URLSearchParams(location.search).has('staff')});
