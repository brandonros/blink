/* Non-waiting snapshots after masked/unmasked precision exceptions. */
static long call(long n,long a,long b,long c){long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
struct extended {unsigned long low;unsigned short high;} __attribute__((packed));
static unsigned char state[512] __attribute__((aligned(16)));
static unsigned char memory[16],record[64];
#define EXEC(instruction) __asm__ volatile("fninit; fldz; fldz; fldz; fldz; fldz; fldz; fldz; fldz; fninit; fldcw %3; fldt %1; fldt %2; " instruction "; fxsave64 %0; fninit":"=m"(state):"m"(y),"m"(x),"m"(control),"m"(memory):"st","st(1)","memory")
__attribute__((noreturn,force_align_arg_pointer)) void _start(void){
 for(unsigned op=0;op<21;op++)for(unsigned mode=0;mode<12;mode++)for(unsigned unmasked=0;unmasked<8;unmasked++)for(unsigned sample=0;sample<((op>=9&&op<=14)||op>=17?32:1);sample++){
  unsigned short control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
  static const unsigned short masks[]={0,32,8,16,24,56,2,63};
  control&=~masks[unmasked];
  struct extended x={0x8000000000000000ul,0x3fff},y={0xc000000000000000ul,0x4000};
  if(op==0)y=(struct extended){0x8000000000000000ul,0x3fb9};
  if(op==1)x=y=(struct extended){0x8ccccccccccccccdul,0x3fff};
  if(op==3)x=(struct extended){0x8000000000000000ul,0x4000};
  if(op==4||op==5||op==7)x=(struct extended){0xc000000000000000ul,0x3fff};
  if(op==6)x=(struct extended){0xaaaaaaaaaaaaaaabul,0x3ffd};
  if(op==9){x=(struct extended){0xfffffffffffffffful,0x7ffe};y=(struct extended){0x8000000000000000ul,0x4000};}
  if(op==10)x=y=(struct extended){0xfffffffffffffffful,0x7ffe};
  if(op==11||op==12)x=(struct extended){0x8000000000000000ul,1};
  if(op==11)y=(struct extended){0x8000000000000000ul,0x3ffe};
  if(op==13){x=(struct extended){0x8000000000000000ul,1};y=(struct extended){0x8000000000000000ul,0x3ffe};}
  if(op==14){x=(struct extended){0xfffffffffffffffful,0x7ffe};y=(struct extended){0x8000000000000000ul,0x4000};}
  if(op==15||op==16){x=(struct extended){1,0};y=(struct extended){0x8000000000000000ul,0x3fff};}
  if(op>=17){
   x=(struct extended){0x8000000000000000ul,0x3fff};
   if(op==18)x=(struct extended){0xfffffffffffffffful,0x7ffe};
   if(op==19)x=(struct extended){1,0};
   if(op==20)x=(struct extended){0x8000000000000000ul,1};
   static const struct extended powers[]={
    {0x8000000000000000ul,0x3fff},{0x8000000000000000ul,0xbfff},
    {0x8000000000000000ul,0x400d},{0x8000000000000000ul,0xc00d},
    {0xc000000000000000ul,0x400d},{0xc000000000000000ul,0xc00d},
    {0x8000000000000000ul,0x400e},{0x8000000000000000ul,0xc00e},
    {0x9c40000000000000ul,0x400e},{0x9c40000000000000ul,0xc00e},
    {0x8000000000000000ul,0x400f},{0x8000000000000000ul,0xc00f},
    {0x8000000000000000ul,0x4fff},{0x8000000000000000ul,0xcfff},
    {0xfffffffffffffffful,0x3fff},{0xfffffffffffffffful,0xbfff}};
   y=powers[sample%16];if(sample>=16)x.high^=0x8000;
  }
  if(sample && op<17){
   unsigned long seed=sample*0x9e3779b97f4a7c15ul;
   seed^=seed>>12;seed^=seed<<25;seed^=seed>>27;
   x.low=seed|0x8000000000000000ul;y.low=(seed*0x2545f4914f6cdd1dul)|0x8000000000000000ul;
   if(sample&1)x.high|=0x8000;
   if(sample&2)y.high|=0x8000;
   if(op==9||op==14)y.high=(y.high&0x8000)|(0x4000+(sample&3));
   if(op==11||op==13)y.high=(y.high&0x8000)|(0x3ffb+(sample&3));
   if(op==12)y.high=(y.high&0x8000)|(0x4000+(sample&3));
   if(sample==1&&(op==11||op==13)){x=(struct extended){0x8000000000000000ul,1};y=(struct extended){0xfffffffffffffffful,0x3ffe};}
  }
  for(int n=0;n<16;n++)memory[n]=0xa5;
  switch(op){
   case 0:EXEC("fadd %%st(1),%%st");break;
   case 1:EXEC("fmul %%st(1),%%st");break;
   case 2:EXEC("fdiv %%st(1),%%st");break;
   case 3:EXEC("fsqrt");break;
   case 4:EXEC("frndint");break;
   case 5:EXEC("fistpl %4");break;
   case 6:EXEC("fstps %4");break;
   case 7:EXEC("fbstp %4");break;
   case 8:EXEC("fdivrp %%st,%%st(1)");break;
   case 9:EXEC("fmul %%st(1),%%st");break;
   case 10:EXEC("fadd %%st(1),%%st");break;
   case 11:EXEC("fmul %%st(1),%%st");break;
   case 12:EXEC("fdiv %%st(1),%%st");break;
   case 13:case 14:EXEC("fmulp %%st,%%st(1)");break;
   case 15:EXEC("fadd %%st(1),%%st");break;
   case 16:EXEC("fmul %%st(1),%%st");break;
   case 17:case 18:case 19:case 20:EXEC("fscale");break;
  }
  for(int n=0;n<64;n++)record[n]=0;
  for(int n=0;n<10;n++){record[n]=state[32+n];record[10+n]=state[48+n];}
  for(int n=0;n<16;n++)record[20+n]=memory[n];
  record[36]=state[2];record[37]=state[3];record[38]=state[4];
  record[40]=op;record[41]=mode;record[42]=unmasked;record[43]=sample;
  if(call(1,1,(long)record,64)!=64){call(231,1,0,0);__builtin_unreachable();}
 }
 call(231,0,0,0);__builtin_unreachable();
}
